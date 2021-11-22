use crate::prelude::graphql::*;
use async_trait::async_trait;
use futures::lock::Mutex;
use lru::LruCache;
use std::sync::Arc;

/// A query planner wrapper that caches results.
///
/// There is no eviction strategy, query plans will be retained forever.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached: Mutex<LruCache<QueryKey, Result<Arc<QueryPlan>, QueryPlannerError>>>,
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    /// Creates a new query planner that cache the results of another [`QueryPlanner`].
    pub fn new(delegate: T) -> CachingQueryPlanner<T> {
        Self {
            delegate,
            cached: Mutex::new(LruCache::new(100)), //XXX 100 must be configurable
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
    ) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        let mut locked_cache = self.cached.lock().await;
        let key = (query.clone(), operation.clone(), options.clone());
        if let Some(value) = locked_cache.get(&key).cloned() {
            return value;
        }

        let value = self
            .delegate
            .get(key.0.clone(), key.1.clone(), key.2.clone())
            .await;
        locked_cache.put(key, value.clone());
        value
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
