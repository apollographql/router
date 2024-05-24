#![allow(missing_docs)] // FIXME

use serde_json_bytes::Value;
use std::pin::Pin;
use tokio_stream::Stream;
use tower::BoxError;

use crate::error::Error;
use crate::graphql;
use crate::json_ext::Path;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::ExecutionParameters;

pub type BoxService<'a> = tower::util::BoxService<Request<'a>, Response, BoxError>;
pub type BoxCloneService<'a> = tower::util::BoxCloneService<Request<'a>, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;
pub(crate) type BoxGqlStream = Pin<Box<dyn Stream<Item = graphql::Response> + Send + Sync>>;

#[derive(Clone)]
#[non_exhaustive]
pub(crate) struct Request<'a>
{
    /// Original request to the Router.
    pub fetch_node: FetchNode,
    pub(crate) parameters: &'a ExecutionParameters<'a>,
    pub(crate) data: &'a Value,
    pub(crate) current_dir: &'a Path,
}

pub(crate) type Response = (Value, Vec<Error>);
