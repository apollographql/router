use crate::CacheCallback;
use futures::lock::Mutex;
use lru::{KeyRef, LruCache};
use std::borrow::Borrow;
use std::cmp::Eq;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use tokio::sync::broadcast::{self, Sender};
use tokio::task::JoinError;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
pub struct CachingMap<E, K, V> {
    // delegate: Option<Box<dyn CacheCallback<E, K, V> + Send + Sync + 'static>>,
    cached: Mutex<LruCache<K, Result<V, E>>>,
    #[allow(clippy::type_complexity)]
    wait_map: Mutex<HashMap<K, Sender<(K, Result<V, E>)>>>,
    cache_limit: usize,
}

impl<E, K, V> fmt::Debug for CachingMap<E, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingMap")
            .field("cache_limit", &self.cache_limit)
            .finish()
    }
}

impl<E, K, V> CachingMap<E, K, V>
where
    E: Error + From<JoinError> + Send + Sync + 'static,
    K: Clone + fmt::Debug + Eq + Hash + Send + Sync + 'static,
    V: fmt::Debug + Send + Sync + 'static,
    KeyRef<K>: Borrow<K>,
    Result<V, E>: Clone,
{
    /// Creates a new CachingMap
    // pub fn new(delegate: Option<Box<dyn CacheCallback<E, K, V>>>, cache_limit: usize) -> Self {
    pub fn new(cache_limit: usize) -> Self {
        Self {
            // delegate,
            cached: Mutex::new(LruCache::new(cache_limit)),
            wait_map: Mutex::new(HashMap::new()),
            cache_limit,
        }
    }

    pub async fn get(
        &self,
        callback: &(dyn CacheCallback<E, K, V> + Send + Sync + 'static),
        key: K,
    ) -> Result<V, E> {
        let mut locked_cache = self.cached.lock().await;
        if let Some(value) = locked_cache.get(&key).cloned() {
            tracing::info!("FOUND in cache: {:?}", &value);
            return value;
        }

        // Holding a lock across the delegated get is a bad idea because
        // the delegate get() calls into v8 for processing of the plan.
        // This would block all other get() requests for a potentially
        // long time.
        // Alternatively, if we don't hold the lock, there is a risk
        // that we will do the work multiple times. This is also
        // sub-optimal.

        // To work around this, we keep a list of keys we are currently
        // processing in the delegate. If we try to get a key on this
        // list, we block and wait for it to complete and then retry.
        //
        // This is more complex than either of the two simple
        // alternatives but succeeds in providing a mechanism where each
        // client only waits for uncached QueryPlans they are going to
        // use AND avoids generating the plan multiple times.

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
                // Our use case is very specific, so we are sure
                // that we won't get any errors here.
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
                // No cache locks are held here
                let value = callback.delegated_get(key.clone()).await;
                // Update our cache
                let mut locked_cache = self.cached.lock().await;
                locked_cache.put(key.clone(), value.clone());
                // Update our wait list
                let mut locked_wait_map = self.wait_map.lock().await;
                locked_wait_map.remove(&key);
                // Let our waiters know
                let broadcast_value = value.clone();
                // Our use case is very specific, so we are sure that
                // we won't get any errors here.
                tokio::task::spawn_blocking(move || {
                    tx.send((key, broadcast_value))
                        .expect("there is always at least one receiver alive, the _rx guard; qed")
                })
                .await?;
                tracing::info!("NOT FOUND in cache: {:?}", &value);
                value
            }
        }
    }

    pub async fn get_with<Fut: Future<Output = Result<V, E>>>(
        &self,
        callback: impl FnOnce(K) -> Fut,
        key: K,
    ) -> Result<V, E> {
        let mut locked_cache = self.cached.lock().await;
        if let Some(value) = locked_cache.get(&key).cloned() {
            tracing::info!("FOUND in cache: {:?}", &value);
            return value;
        }

        // Holding a lock across the delegated get is a bad idea because
        // the delegate get() calls into v8 for processing of the plan.
        // This would block all other get() requests for a potentially
        // long time.
        // Alternatively, if we don't hold the lock, there is a risk
        // that we will do the work multiple times. This is also
        // sub-optimal.

        // To work around this, we keep a list of keys we are currently
        // processing in the delegate. If we try to get a key on this
        // list, we block and wait for it to complete and then retry.
        //
        // This is more complex than either of the two simple
        // alternatives but succeeds in providing a mechanism where each
        // client only waits for uncached QueryPlans they are going to
        // use AND avoids generating the plan multiple times.

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
                // Our use case is very specific, so we are sure
                // that we won't get any errors here.
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
                // No cache locks are held here
                let value = (callback)(key.clone()).await;
                // Update our cache
                let mut locked_cache = self.cached.lock().await;
                locked_cache.put(key.clone(), value.clone());
                // Update our wait list
                let mut locked_wait_map = self.wait_map.lock().await;
                locked_wait_map.remove(&key);
                // Let our waiters know
                let broadcast_value = value.clone();
                // Our use case is very specific, so we are sure that
                // we won't get any errors here.
                tokio::task::spawn_blocking(move || {
                    tx.send((key, broadcast_value))
                        .expect("there is always at least one receiver alive, the _rx guard; qed")
                })
                .await?;
                tracing::info!("NOT FOUND in cache: {:?}", &value);
                value
            }
        }
    }

    pub async fn get_hot_keys(&self) -> Vec<K> {
        let locked_cache = self.cached.lock().await;
        locked_cache
            .iter()
            .take(self.cache_limit / 5)
            .map(|(key, _value)| key.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryPlannerError;
    use test_log::test;

    #[test(tokio::test)]
    async fn it_should_enforce_cache_limits() {
        let cm: CachingMap<QueryPlannerError, usize, usize> = CachingMap::new(13);

        let q = |key: usize| async move { Ok(key) };
        for i in 0..14 {
            cm.get_with(q, i).await;
        }
        let guard = cm.cached.lock().await;
        println!("{:?}", guard);
        assert_eq!(guard.len(), 13);
    }
}
