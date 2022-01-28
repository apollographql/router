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
