use crate::prelude::graphql::*;
use crate::CacheCallback;
use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;

type PlanResult = Result<Arc<QueryPlan>, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cm: CachingMap<QueryPlannerError, QueryKey, Arc<QueryPlan>>,
}

impl<T: QueryPlanner> fmt::Debug for CachingQueryPlanner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingQueryPlanner").finish()
    }
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    /// Creates a new query planner that cache the results of another [`QueryPlanner`].
    pub fn new(delegate: T, plan_cache_limit: usize) -> CachingQueryPlanner<T> {
        let cm = CachingMap::new(plan_cache_limit);
        Self { delegate, cm }
    }
}

#[async_trait]
impl<T: QueryPlanner> CacheCallback<QueryPlannerError, QueryKey, Arc<QueryPlan>>
    for CachingQueryPlanner<T>
{
    async fn delegated_get(&self, key: QueryKey) -> Result<Arc<QueryPlan>, QueryPlannerError> {
        self.delegate.get(key.0, key.1, key.2).await
    }
}

#[async_trait]
impl<T: QueryPlanner + 'static> QueryPlanner for CachingQueryPlanner<T> {
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> PlanResult {
        let key = (query, operation, options);
        self.cm.get(self, key).await
    }

    async fn get_hot_keys(&self) -> Vec<QueryKey> {
        self.cm.get_hot_keys().await
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
