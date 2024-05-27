#![allow(missing_docs)] // FIXME

use std::collections::HashMap;
// use std::pin::Pin;
use std::sync::Arc;

use apollo_compiler::NodeStr;
use serde_json_bytes::Value;
use tokio::sync::broadcast;

// use tokio_stream::Stream;
// use tower::BoxError;
use crate::error::Error;
// use crate::graphql;
use crate::graphql::Request as GraphQLRequest;
use crate::json_ext::Path;
use crate::query_planner::fetch::FetchNode;

// pub type BoxService<'a> = tower::util::BoxService<Request<'a>, Response, BoxError>;
// pub type BoxCloneService<'a> = tower::util::BoxCloneService<Request<'a>, Response, BoxError>;
// pub type ServiceResult = Result<Response, BoxError>;
// pub(crate) type BoxGqlStream = Pin<Box<dyn Stream<Item = graphql::Response> + Send + Sync>>;

#[derive(Clone)]
#[non_exhaustive]
pub(crate) struct Request<'a> {
    pub(crate) fetch_node: FetchNode,
    pub(crate) supergraph_request: &'a Arc<http::Request<GraphQLRequest>>,
    pub(crate) data: &'a Value,
    pub(crate) current_dir: &'a Path,
    pub(crate) deferred_fetches: &'a HashMap<NodeStr, broadcast::Sender<(Value, Vec<Error>)>>,
}

pub(crate) type Response = (Value, Vec<Error>);
