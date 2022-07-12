use async_trait::async_trait;

use crate::error::CacheResolverError;
use crate::error::QueryPlannerError;
use crate::query_planner::QueryPlanOptions;
use crate::services::QueryPlannerContent;

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
pub(crate) trait QueryPlanner: Send + Sync {
    /// Returns a query plan given the query, operation and options.
    /// Implementations may cache query plans.
    #[must_use = "query plan result must be used"]
    async fn get(&self, key: QueryKey) -> Result<QueryPlannerContent, QueryPlannerError>;
}
