use std::collections::HashMap;

/// Caching query planner that caches responses from a delegate.
use crate::model::QueryPlan;
use crate::{QueryPlanOptions, QueryPlanner, QueryPlannerError};

/// A query planner decorator that caches results.
/// There is no eviction strategy, query plans will be retained forever.
#[derive(Debug)]
pub struct CachingQueryPlanner<T: QueryPlanner> {
    delegate: T,
    cached:
        HashMap<(String, String, crate::QueryPlanOptions), Result<QueryPlan, QueryPlannerError>>,
}

impl<T: QueryPlanner> CachingQueryPlanner<T> {
    fn new(delegate: T) -> CachingQueryPlanner<T> {
        CachingQueryPlanner {
            delegate,
            cached: HashMap::new(),
        }
    }
}

impl<T: QueryPlanner> crate::QueryPlanner for CachingQueryPlanner<T> {
    fn get(
        &mut self,
        query: &str,
        operation: &str,
        options: QueryPlanOptions,
    ) -> Result<QueryPlan, QueryPlannerError> {
        let delegate = &mut self.delegate;
        self.cached
            .entry((query.into(), operation.into(), options.clone()))
            .or_insert_with(|| delegate.get(query, operation, options.clone()))
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
    use super::*;
    use crate::MockQueryPlanner;

    #[test]
    fn test_plan() {
        let mut delegate = MockQueryPlanner::new();
        delegate
            .expect_get()
            .times(2)
            .return_const(Err(QueryPlannerError::ParseError {
                parse_errors: "".to_owned(),
            }));

        let mut planner = delegate.with_caching();

        for _ in 0..5 {
            assert!(planner
                .get("query1", "", QueryPlanOptions::default())
                .is_err());
        }
        assert!(planner
            .get("query2", "", QueryPlanOptions::default())
            .is_err());
    }
}
