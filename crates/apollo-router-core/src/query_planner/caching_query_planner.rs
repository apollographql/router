use crate::prelude::graphql::*;
use async_trait::async_trait;
use bus::Bus;
use futures::lock::Mutex;
use lru::LruCache;
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached: Mutex<LruCache<QueryKey, Result<Arc<QueryPlan>, QueryPlannerError>>>,
    wait_list: Mutex<Vec<QueryKey>>,
    bus: Mutex<Bus<QueryKey>>,
}

impl<T: QueryPlanner> fmt::Debug for CachingQueryPlanner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingQueryPlanner")
            .field("delegate", &self.delegate)
            .field("cached", &self.cached)
            .field("wait_list", &self.wait_list)
            .finish()
    }
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    /// Creates a new query planner that cache the results of another [`QueryPlanner`].
    pub fn new(delegate: T) -> CachingQueryPlanner<T> {
        Self {
            delegate,
            cached: Mutex::new(LruCache::new(100)), //XXX 100 must be configurable
            wait_list: Mutex::new(vec![]),
            bus: Mutex::new(Bus::new(1000)),
        }
    }
}

#[async_trait]
impl<T: QueryPlanner + 'static> QueryPlanner for CachingQueryPlanner<T> {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        let mut locked_cache = self.cached.lock().await;
        let key = (query.clone(), operation.clone(), options.clone());
        if let Some(value) = locked_cache.get(&key).cloned() {
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

        drop(locked_cache);

        let mut locked_wait_list = self.wait_list.lock().await;

        if locked_wait_list.contains(&key) {
            // Register interest on bus
            let mut locked_bus = self.bus.lock().await;
            let rx = Arc::new(StdMutex::new(locked_bus.add_rx()));
            drop(locked_bus);
            drop(locked_wait_list); // Drop wait list lock after registering
            loop {
                let my_rx = rx.clone();
                // Have to spawn a blocking task or we will deadlock
                let msg = tokio::task::spawn_blocking(move || {
                    let mut locked_rx = my_rx.lock().unwrap();
                    locked_rx.recv().map_err(QueryPlannerError::CacheError)
                })
                .await??;
                if msg == key {
                    let mut locked_cache = self.cached.lock().await;
                    if let Some(value) = locked_cache.get(&key).cloned() {
                        return value;
                    }
                    drop(locked_cache);
                }
            }
        } else {
            locked_wait_list.push(key.clone());
            drop(locked_wait_list);
            // This is the potentially high duration operation
            let value = self
                .delegate
                .get(key.0.clone(), key.1.clone(), key.2.clone())
                .await;
            // Update our cache
            let mut locked_cache = self.cached.lock().await;
            locked_cache.put(key.clone(), value.clone());
            // Update our wait list
            let mut locked_wait_list = self.wait_list.lock().await;
            locked_wait_list.retain(|x| x != &key);
            // Let our waiters know
            let mut locked_bus = self.bus.lock().await;
            locked_bus.broadcast(key);
            value
        }
    }

    async fn get_hot_keys(&self) -> Vec<QueryKey> {
        let locked_cache = self.cached.lock().await;
        let mut results = vec![];
        //XXX 10 must be configurable
        for (key, _value) in locked_cache.iter().take(10) {
            results.push(key.clone());
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::{mock, predicate::*};
    use router_bridge::plan::PlanningErrors;
    use std::sync::Arc;
    use test_env_log::test;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_get(
                &self,
                query: String,
                operation: Option<String>,
                options: QueryPlanOptions,
            ) -> Result<Arc<QueryPlan>, QueryPlannerError>;
        }
    }

    #[async_trait]
    impl QueryPlanner for MockMyQueryPlanner {
        async fn get(
            &self,
            query: String,
            operation: Option<String>,
            options: QueryPlanOptions,
        ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
            self.sync_get(query, operation, options)
        }

        async fn get_hot_keys(&self) -> Vec<QueryKey> {
            vec![]
        }
    }

    #[test(tokio::test)]
    async fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        delegate
            .expect_sync_get()
            .times(2)
            .return_const(Err(QueryPlannerError::PlanningErrors(Arc::new(
                PlanningErrors { errors: Vec::new() },
            ))));

        let planner = delegate.with_caching();

        for _ in 0..5 {
            assert!(planner
                .get(
                    "query1".into(),
                    Some("".into()),
                    QueryPlanOptions::default()
                )
                .await
                .is_err());
        }
        assert!(planner
            .get(
                "query2".into(),
                Some("".into()),
                QueryPlanOptions::default()
            )
            .await
            .is_err());
    }
}
