#![allow(missing_docs)] // FIXME

use std::sync::Arc;

use apollo_federation::sources::connect::query_plan::FetchNode;
use serde_json_bytes::Value;
use tower::BoxError;

// use tokio_stream::Stream;
// use tower::BoxError;
use crate::error::Error;
// use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::json_ext::Path;
use crate::query_planner::fetch::Variables;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;
// pub type BoxCloneService<'a> = tower::util::BoxCloneService<Request<'a>, Response, BoxError>;
// pub type ServiceResult = Result<Response, BoxError>;
// pub(crate) type BoxGqlStream = Pin<Box<dyn Stream<Item = graphql::Response> + Send + Sync>>;

#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) context: Context,
    pub(crate) fetch_node: FetchNode,
    pub(crate) _supergraph_request: Arc<http::Request<GraphQLRequest>>,
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
            _supergraph_request: supergraph_request,
            variables,
            current_dir,
        }
    }
}
