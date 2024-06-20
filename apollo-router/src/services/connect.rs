#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use apollo_compiler::NodeStr;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::error::Error;
use crate::graphql::Request as GraphQLRequest;
use crate::query_planner::fetch::Variables;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) service_name: NodeStr,
    #[allow(dead_code)]
    pub(crate) context: Context,
    pub(crate) operation_str: String,
    pub(crate) _supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
}

pub(crate) type Response = (Value, Vec<Error>);

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        service_name: NodeStr,
        context: Context,
        operation_str: String,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
    ) -> Self {
        Self {
            service_name,
            context,
            operation_str,
            _supergraph_request: supergraph_request,
            variables,
        }
    }
}
