use crate::prelude::graphql::*;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;

/// A cache key.
///
/// This type consists of a query string, an optional operation string and the
/// [`QueryPlanOptions`].
type CacheKey = (String, Option<String>, QueryPlanOptions);

/// A query planner wrapper that caches results.
///
/// There is no eviction strategy, query plans will be retained forever.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached: Mutex<HashMap<CacheKey, Result<QueryPlan, QueryPlannerError>>>,
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    /// Creates a new query planner that cache the results of another [`QueryPlanner`].
    pub fn new(delegate: T) -> CachingQueryPlanner<T> {
        Self {
            delegate,
            cached: Default::default(),
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
    ) -> Result<QueryPlan, QueryPlannerError> {
        if let Some(value) = self
            .cached
            .lock()
            .get(&(query.clone(), operation.clone(), options.clone()))
            .cloned()
        {
            return value;
        }

        let value = self
            .delegate
            .get(query.clone(), operation.clone(), options.clone())
            .await;
        self.cached
            .lock()
            .insert((query, operation, options), value.clone());
        value
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
            ) -> Result<QueryPlan, QueryPlannerError>;
        }
    }

    #[async_trait]
    impl QueryPlanner for MockMyQueryPlanner {
        async fn get(
            &self,
            query: String,
            operation: Option<String>,
            options: QueryPlanOptions,
        ) -> Result<QueryPlan, QueryPlannerError> {
            self.sync_get(query, operation, options)
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
