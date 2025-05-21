//! Connect service request and response types.

use std::fmt::Debug;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::validation::Valid;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;
use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::query_planner::fetch::Variables;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) service_name: Arc<str>,
    pub(crate) context: Context,
    pub(crate) operation: Arc<Valid<ExecutableDocument>>,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) keys: Option<Valid<FieldSet>>,
}

impl Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("service_name", &self.service_name)
            .field("context", &self.context)
            .field("operation", &self.operation)
            .field("supergraph_request", &self.supergraph_request)
            .field("variables", &self.variables.variables)
            .finish()
    }
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
        keys: Option<Valid<FieldSet>>,
    ) -> Self {
        Self {
            service_name,
            context,
            operation,
            supergraph_request,
            variables,
            keys,
        }
    }
}
