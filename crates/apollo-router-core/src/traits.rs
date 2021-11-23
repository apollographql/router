use crate::prelude::graphql::*;
use async_trait::async_trait;
use futures::prelude::*;
use std::sync::Arc;
use std::{fmt::Debug, pin::Pin};

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
pub trait Fetcher: Send + Sync + Debug {
    /// Constructs a stream of responses.
    #[must_use = "streams do nothing unless polled"]
    fn stream(&self, request: Request) -> Pin<Box<dyn Future<Output = ResponseStream> + Send>>;
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
    Self: Sized + QueryPlanner,
{
    /// Wrap this query planner in a caching decorator.
    /// The original query planner is consumed.
    fn with_caching(self) -> CachingQueryPlanner<Self> {
        CachingQueryPlanner::new(self)
    }
}

impl<T: ?Sized> WithCaching for T where T: QueryPlanner + Sized {}

/// An object that accepts a [`Request`] and allow creating [`Route`]'s.
///
/// The call to the function will either succeeds and return a [`Route`] or it will fail and return
/// a [`ResponseStream`] that can be returned immediately to the user. This is because GraphQL does
/// not use the HTTP error codes, therefore it always return a response even if it fails.
pub trait Router<T: Route>: Send + Sync + Debug {
    fn create_route<'a>(
        &'a self,
        request: &'a Request,
    ) -> future::BoxFuture<'a, Result<T, ResponseStream>>;
}

/// An object that can be executed to return a [`ResponseStream`].
pub trait Route: Send + Debug {
    fn execute(self, request: Arc<Request>) -> future::BoxFuture<'static, ResponseStream>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::*;

    assert_obj_safe!(ServiceRegistry);
    assert_obj_safe!(Fetcher);
    assert_obj_safe!(QueryPlanner);
}
