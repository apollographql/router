mod execution_service;
mod router_service;

pub use self::execution_service::*;
pub use self::router_service::*;
use crate::prelude::graphql::*;
use futures::future::BoxFuture;
use static_assertions::assert_impl_all;
use std::sync::Arc;

// the parsed graphql Request, HTTP headers and contextual data for extensions
pub struct RouterRequest {
    pub http_request: http::Request<Request>,

    // Context for extension
    pub context: Context<()>,
}

impl From<http::Request<Request>> for RouterRequest {
    fn from(http_request: http::Request<Request>) -> Self {
        Self {
            http_request,
            context: Context::new(),
        }
    }
}

assert_impl_all!(PlannedRequest: Send);
/// TODO confusing name since this is a Response
pub struct PlannedRequest {
    pub query_plan: Arc<QueryPlan>,

    pub context: Context,
}

assert_impl_all!(SubgraphRequest: Send);
pub struct SubgraphRequest {
    pub http_request: http::Request<Request>,

    pub context: Context,
}

assert_impl_all!(QueryPlannerRequest: Send);
pub struct QueryPlannerRequest {
    pub options: QueryPlanOptions,

    pub context: Context,
}

assert_impl_all!(RouterResponse: Send);
pub struct RouterResponse {
    pub response: http::Response<Response>,

    pub context: Context,
}

impl AsRef<Request> for http::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

// This code is inspired from:
// https://users.rust-lang.org/t/stalemated-between-impl-and-dyn-vicious-cycle/65546/3
//
// I only added `clone_box()` which is itself inspired from:
// https://docs.rs/tower/latest/src/tower/util/boxed_clone.rs.html#111-130
pub(crate) trait DynCloneService<Request>: Send + Sync {
    type Response;
    type Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>>;
    fn call(&mut self, req: Request) -> BoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn clone_box(
        &self,
    ) -> Box<dyn DynCloneService<Request, Response = Self::Response, Error = Self::Error>>;
}

impl<T, R> DynCloneService<R> for T
where
    T: tower::Service<R> + Clone + Send + Sync + 'static,
    T::Future: Send + 'static,
{
    type Response = <T as tower::Service<R>>::Response;
    type Error = <T as tower::Service<R>>::Error;

    fn ready<'a>(&'a mut self) -> BoxFuture<'a, Result<(), Self::Error>> {
        Box::pin(futures::future::poll_fn(move |cx| self.poll_ready(cx)))
    }
    fn call(&mut self, req: R) -> BoxFuture<'static, Result<Self::Response, Self::Error>> {
        let fut = tower::Service::call(self, req);
        Box::pin(fut)
    }
    fn clone_box(
        &self,
    ) -> Box<dyn DynCloneService<R, Response = Self::Response, Error = Self::Error>> {
        Box::new(self.clone())
    }
}
