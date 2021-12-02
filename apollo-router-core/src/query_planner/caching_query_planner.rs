use crate::prelude::graphql::*;
use crate::CacheResolver;
use async_trait::async_trait;
use std::marker::PhantomData;
use std::sync::Arc;

type PlanResult = Result<Arc<QueryPlan>, QueryPlannerError>;

/// A query planner wrapper that caches results.
///
/// The query planner performs LRU caching.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    cm: CachingMap<QueryKey, Arc<QueryPlan>>,
    phantom: PhantomData<T>,
}

/// A resolver for cache misses
struct CachingQueryPlannerResolver<T: QueryPlanner> {
    delegate: T,
}

impl<T: QueryPlanner + 'static> CachingQueryPlanner<T> {
    /// Creates a new query planner that caches the results of another [`QueryPlanner`].
    pub fn new(delegate: T, plan_cache_limit: usize) -> CachingQueryPlanner<T> {
        let resolver = CachingQueryPlannerResolver { delegate };
        let cm = CachingMap::new(Box::new(resolver), plan_cache_limit);
        Self {
            cm,
            phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T: QueryPlanner> CacheResolver<QueryKey, Arc<QueryPlan>> for CachingQueryPlannerResolver<T> {
    async fn retrieve(&self, key: QueryKey) -> Result<Arc<QueryPlan>, CacheResolverError> {
        self.delegate
            .get(key.0, key.1, key.2)
            .await
            .map_err(|err| err.into())
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
        let key = (query, operation, options);
        self.cm.get(key).await.map_err(|err| err.into())
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
