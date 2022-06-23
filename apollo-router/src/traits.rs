use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::QueryPlanOptions;
use crate::services::QueryPlannerContent;
use async_trait::async_trait;
use std::fmt::Debug;

/// A cache resolution trait.
///
/// Clients of CachingMap are required to provider a resolver during Map creation. The resolver
/// will be used to find values for cache misses. A Result is expected, because retrieval may fail.
#[async_trait]
pub(crate) trait CacheResolver<K, V> {
    async fn retrieve(&self, key: K) -> Result<V, CacheResolverError>;
}

/// A planner key.
///
/// This type consists of a query string, an optional operation string and the
/// [`QueryPlanOptions`].
pub(crate) type QueryKey = (String, Option<String>, QueryPlanOptions);

/// QueryPlanner can be used to plan queries.
///
/// Implementations may cache query plans.
#[async_trait]
pub(crate) trait QueryPlanner: Send + Sync + Debug {
    /// Returns a query plan given the query, operation and options.
    /// Implementations may cache query plans.
    #[must_use = "query plan result must be used"]
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<QueryPlannerContent, QueryPlannerError>;
}

/// With caching trait.
///
/// Adds with_caching to any query planner.
pub(crate) trait WithCaching: QueryPlanner
where
    Self: Sized + QueryPlanner + 'static,
{
    /// Wrap this query planner in a caching decorator.
    /// The original query planner is consumed.
    fn with_caching(self, plan_cache_limit: usize) -> CachingQueryPlanner<Self> {
        CachingQueryPlanner::new(self, plan_cache_limit)
    }
}

impl<T: ?Sized> WithCaching for T where T: QueryPlanner + Sized + 'static {}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::*;

    assert_obj_safe!(QueryPlanner);
}
