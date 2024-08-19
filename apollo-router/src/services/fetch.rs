//! Fetch request and response types.

use std::sync::Arc;

use serde_json_bytes::Value;
use tower::BoxError;

use crate::error::Error;
use crate::graphql::Request as GraphQLRequest;
use crate::json_ext::Path;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Variables;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) context: Context,
    pub(crate) fetch_node: FetchNode,
    pub(crate) supergraph_request: Arc<http::Request<GraphQLRequest>>,
    pub(crate) variables: Variables,
    pub(crate) current_dir: Path,
}

pub(crate) type Response = (Value, Vec<Error>);

#[buildstructor::buildstructor]
impl Request {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder(visibility = "pub")]
    fn new(
        context: Context,
        fetch_node: FetchNode,
        supergraph_request: Arc<http::Request<GraphQLRequest>>,
        variables: Variables,
        current_dir: Path,
    ) -> Self {
        Self {
            context,
            fetch_node,
            supergraph_request,
            variables,
            current_dir,
        }
    }
}
