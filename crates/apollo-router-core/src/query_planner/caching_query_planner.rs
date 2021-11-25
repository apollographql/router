use crate::prelude::graphql::*;
use async_trait::async_trait;
use futures::lock::Mutex;
use lru::LruCache;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast::{self, Sender};

type PlanResult = Result<Arc<QueryPlan>, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached: Mutex<LruCache<QueryKey, PlanResult>>,
    wait_map: Mutex<HashMap<QueryKey, Sender<(QueryKey, PlanResult)>>>,
    plan_cache_limit: usize,
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    /// Creates a new query planner that cache the results of another [`QueryPlanner`].
    pub fn new(delegate: T, plan_cache_limit: usize) -> CachingQueryPlanner<T> {
        Self {
            delegate,
            cached: Mutex::new(LruCache::new(plan_cache_limit)),
            wait_map: Mutex::new(HashMap::new()),
            plan_cache_limit,
        }
    }
}

#[async_trait]
impl<T: QueryPlanner> QueryPlanner for CachingQueryPlanner<T> {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> PlanResult {
        let mut locked_cache = self.cached.lock().await;
        let key = (query, operation, options);
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

        let mut locked_wait_map = self.wait_map.lock().await;

        match locked_wait_map.get_mut(&key) {
            Some(waiter) => {
                // Register interest in key
                let mut receiver = waiter.subscribe();
                drop(locked_wait_map);
                let msg = receiver.recv().await?;
                assert_eq!(msg.0, key);
                msg.1
            }
            None => {
                let (tx, _rx) = broadcast::channel(10);
                locked_wait_map.insert(key.clone(), tx.clone());
                drop(locked_wait_map);
                // This is the potentially high duration operation
                let value = self
                    .delegate
                    .get(key.0.clone(), key.1.clone(), key.2.clone())
                    .await;
                // Update our cache
                let mut locked_cache = self.cached.lock().await;
                locked_cache.put(key.clone(), value.clone());
                // Update our wait list
                let mut locked_wait_map = self.wait_map.lock().await;
                locked_wait_map.remove(&key);
                // Let our waiters know
                let broadcast_value = value.clone();
                tokio::task::spawn_blocking(move || tx.send((key, broadcast_value))).await??;
                value
            }
        }
    }

    async fn get_hot_keys(&self) -> Vec<QueryKey> {
        let locked_cache = self.cached.lock().await;
        locked_cache
            .iter()
            .take(self.plan_cache_limit / 5)
            .map(|(key, _value)| key.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::{mock, predicate::*};
    use router_bridge::plan::PlanningErrors;
    use std::sync::Arc;
    use test_log::test;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {
            fn sync_get(
                &self,
                query: String,
                operation: Option<String>,
                options: QueryPlanOptions,
            ) -> PlanResult;
        }
    }

    #[async_trait]
    impl QueryPlanner for MockMyQueryPlanner {
        async fn get(
            &self,
            query: String,
            operation: Option<String>,
            options: QueryPlanOptions,
        ) -> PlanResult {
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

        let planner = delegate.with_caching(10);

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
