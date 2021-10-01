use crate::prelude::graphql::*;
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

impl<T: QueryPlanner> QueryPlanner for CachingQueryPlanner<T> {
    fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<QueryPlan, QueryPlannerError> {
        self.cached
            .lock()
            .entry((query.clone(), operation.clone(), options.clone()))
            .or_insert_with(|| self.delegate.get(query, operation, options))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::{mock, predicate::*};
    use std::sync::Arc;

    mock! {
        #[derive(Debug)]
        MyQueryPlanner {}

        impl QueryPlanner for MyQueryPlanner {
            fn get(
                &self,
                query: String,
                operation: Option<String>,
                options: QueryPlanOptions,
            ) -> Result<QueryPlan, QueryPlannerError>;
        }
    }

    #[test]
    fn test_plan() {
        let mut delegate = MockMyQueryPlanner::new();
        let serde_json_error = serde_json::from_slice::<()>(&[]).unwrap_err();
        delegate
            .expect_get()
            .times(2)
            .return_const(Err(QueryPlannerError::ParseError(Arc::new(
                serde_json_error,
            ))));

        let planner = delegate.with_caching();

        for _ in 0..5 {
            assert!(planner
                .get(
                    "query1".into(),
                    Some("".into()),
                    QueryPlanOptions::default()
                )
                .is_err());
        }
        assert!(planner
            .get(
                "query2".into(),
                Some("".into()),
                QueryPlanOptions::default()
            )
            .is_err());
    }
}
