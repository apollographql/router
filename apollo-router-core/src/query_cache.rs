use crate::prelude::graphql::*;
use futures::lock::Mutex;
use lru::LruCache;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// A cache for parsed GraphQL queries.
#[derive(Debug)]
pub struct QueryCache {
    cached: Mutex<LruCache<String, Option<Arc<Query>>>>,
    #[allow(clippy::type_complexity)]
    wait_map: Mutex<HashMap<String, broadcast::Sender<(String, Option<Arc<Query>>)>>>,
    schema: Arc<Schema>,
}

impl QueryCache {
    /// Instantiate a new cache for parsed GraphQL queries.
    pub fn new(cache_limit: usize, schema: Arc<Schema>) -> Self {
        Self {
            cached: Mutex::new(LruCache::new(cache_limit)),
            wait_map: Mutex::new(HashMap::new()),
            schema,
        }
    }

    /// Attempt to parse a string to a [`Query`] using cache if possible.
    pub async fn get_query(&self, query: impl AsRef<str>) -> Option<Arc<Query>> {
        let mut locked_cache = self.cached.lock().await;
        let key = query.as_ref().to_string();
        if let Some(value) = locked_cache.get(&key).cloned() {
            return value;
        }

        // Holding a lock across the query parsing tasks is a bad idea because this would block all
        // other get() requests for a potentially long time.
        //
        // Alternatively, if we don't hold the lock, there is a risk that we will do the work
        // multiple times. This is also sub-optimal.

        // To work around this, we keep a list of keys we are currently processing.  If we try to
        // get a key on this list, we block and wait for it to complete and then retry.
        //
        // This is more complex than either of the two simple alternatives but succeeds in
        // providing a mechanism where each client only waits for uncached Query they are going to
        // use AND avoids generating the query multiple times.

        let mut locked_wait_map = self.wait_map.lock().await;

        // We must only drop the locked cache after we have locked the
        // wait map. Otherwise,we might get a race that causes us to
        // miss a broadcast.
        drop(locked_cache);

        match locked_wait_map.get_mut(&key) {
            Some(waiter) => {
                // Register interest in key
                let mut receiver = waiter.subscribe();
                drop(locked_wait_map);
                let (recv_key, recv_plan) = receiver.recv().await.expect(
                    "the sender won't ever be dropped before all the receivers finish; qed",
                );
                debug_assert_eq!(recv_key, key);
                recv_plan
            }
            None => {
                let (tx, _rx) = broadcast::channel(1);
                locked_wait_map.insert(key.clone(), tx.clone());
                drop(locked_wait_map);
                // This is the potentially high duration operation
                // No locks are held here
                let query_parsing_future = {
                    let query = query.as_ref().to_string();
                    let schema = Arc::clone(&self.schema);
                    tokio::task::spawn_blocking(move || Query::parse(query, &schema))
                };
                let parsed_query = match query_parsing_future.await {
                    Ok(res) => res.map(Arc::new),
                    // Silently ignore cancelled tasks (never happen for blocking tasks).
                    Err(err) if err.is_cancelled() => None,
                    Err(err) => {
                        failfast_debug!("Parsing query task failed: {}", err);
                        None
                    }
                };
                // Update our cache
                let mut locked_cache = self.cached.lock().await;
                locked_cache.put(key.clone(), parsed_query.clone());
                // Update our wait list
                let mut locked_wait_map = self.wait_map.lock().await;
                locked_wait_map.remove(&key);
                // Let our waiters know
                let broadcast_value = parsed_query.clone();
                match tokio::task::spawn_blocking(move || {
                    let _ = tx
                        .send((key, broadcast_value))
                        .expect("there is always at least one receiver alive, the _rx guard; qed");
                })
                .await
                {
                    Ok(()) => parsed_query,
                    Err(err) => {
                        failfast_debug!("Parsing query task failed: {}", err);
                        None
                    }
                }
            }
        }
    }
}
