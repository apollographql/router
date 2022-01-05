use crate::prelude::graphql::*;
use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;

/// A cache resolution trait.
///
/// Clients of CachingMap are required to provider a resolver during Map creation. The resolver
/// will be used to find values for cache misses. A Result is expected, because retrieval may fail.
#[async_trait]
pub trait CacheResolver<K, V> {
    async fn retrieve(&self, key: K) -> Result<V, CacheResolverError>;
}

/// A planner key.
///
/// This type consists of a query string, an optional operation string and the
/// [`QueryPlanOptions`].
pub(crate) type QueryKey = (String, Option<String>, QueryPlanOptions);

/// Maintains a map of services to fetchers.
pub trait ServiceRegistry: Send + Sync + Debug {
    /// Get a fetcher for a service.
    fn get(&self, service: &str) -> Option<&(dyn Fetcher)>;

    /// Get a fetcher for a service.
    fn has(&self, service: &str) -> bool;
}

/// A fetcher is responsible for turning a graphql request into a stream of responses.
///
/// The goal of this trait is to hide the implementation details of fetching a stream of graphql responses.
/// We can then create multiple implementations that can be plugged into federation.
#[async_trait]
pub trait Fetcher: Send + Sync + Debug {
    /// Constructs a stream of responses.
    #[must_use = "streams do nothing unless polled"]
    async fn stream(&self, request: Request) -> ResponseStream;
}

/// QueryPlanner can be used to plan queries.
///
/// Implementations may cache query plans.
#[async_trait]
pub trait QueryPlanner: Send + Sync + Debug {
    /// Returns a query plan given the query, operation and options.
    /// Implementations may cache query plans.
    #[must_use = "query plan result must be used"]
    async fn get(
        &self,
        query: String,
        operation: Option<String>,
        options: QueryPlanOptions,
    ) -> Result<Arc<QueryPlan>, QueryPlannerError>;
}

/// With caching trait.
///
/// Adds with_caching to any query planner.
pub trait WithCaching: QueryPlanner
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

/// An object that accepts a [`Request`] and allow creating [`PreparedQuery`]'s.
///
/// The call to the function will either succeeds and return a [`PreparedQuery`] or it will fail and return
/// a [`ResponseStream`] that can be returned immediately to the user. This is because GraphQL does
/// not use the HTTP error codes, therefore it always return a response even if it fails.
#[async_trait::async_trait]
pub trait Router<T: PreparedQuery>: Send + Sync + Debug {
    async fn prepare_query(&self, request: &Request) -> Result<T, ResponseStream>;
}

/// An object that can be executed to return a [`ResponseStream`].
#[async_trait::async_trait]
pub trait PreparedQuery: Send + Debug {
    async fn execute(self, request: Arc<Request>) -> ResponseStream;
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::*;

    assert_obj_safe!(ServiceRegistry);
    assert_obj_safe!(Fetcher);
    assert_obj_safe!(QueryPlanner);
}
