//! Connect service request and response types.

use std::sync::Arc;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::query_planner::fetch::Variables;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) operation: Arc<Valid<ExecutableDocument>>,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
}

assert_impl_all!(Response: Send);
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Response {
    pub(crate) response: http::Response<graphql::Response>,
}

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        service_name: Arc<str>,
        context: Context,
        operation: Arc<Valid<ExecutableDocument>>,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
    ) -> Self {
        Self {
            service_name,
            context,
            operation,
            supergraph_request,
            variables,
        }
    }
}
