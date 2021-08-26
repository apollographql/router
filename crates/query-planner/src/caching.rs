use crate::model::QueryPlan;
use crate::{QueryPlanOptions, QueryPlanner, QueryPlannerError};
use parking_lot::Mutex;
use std::collections::HashMap;

type CacheKey = (String, Option<String>, crate::QueryPlanOptions);

/// A query planner wrapper that caches results.
///
/// There is no eviction strategy, query plans will be retained forever.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached: Mutex<HashMap<CacheKey, Result<QueryPlan, QueryPlannerError>>>,
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    fn new(delegate: T) -> CachingQueryPlanner<T> {
        Self {
            delegate,
            cached: Default::default(),
        }
    }
}

impl<T: QueryPlanner> crate::QueryPlanner for CachingQueryPlanner<T> {
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

/// With caching trait. Adds with_caching to any query planner.
pub trait WithCaching: QueryPlanner
where
    Self: Sized + QueryPlanner,
{
    /// Wrap this query planner in a caching decorator.
    /// The original query planner is consumed.
    fn with_caching(self) -> CachingQueryPlanner<Self> {
        CachingQueryPlanner::new(self)
    }
}
impl<T: ?Sized> WithCaching for T where T: QueryPlanner + Sized {}

#[cfg(test)]
mod tests {
    use crate::MockQueryPlanner;

    use super::*;

    #[test]
    fn test_plan() {
        let mut delegate = MockQueryPlanner::new();
        delegate
            .expect_get()
            .times(2)
            .return_const(Err(QueryPlannerError::ParseError {
                parse_errors: "".into(),
            }));

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
