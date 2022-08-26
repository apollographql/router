//! Implementation of the various steps in the router's processing pipeline.

use std::sync::Arc;

pub(crate) use self::execution_service::*;
pub(crate) use self::query_planner::*;
pub(crate) use self::subgraph_service::*;
pub(crate) use self::supergraph_service::*;
use crate::graphql::Request;
use crate::http_ext;
pub use crate::http_ext::TryIntoHeaderName;
pub use crate::http_ext::TryIntoHeaderValue;
pub(crate) use crate::services::execution::Request as ExecutionRequest;
pub(crate) use crate::services::execution::Response as ExecutionResponse;
pub(crate) use crate::services::query_planner::Request as QueryPlannerRequest;
pub(crate) use crate::services::query_planner::Response as QueryPlannerResponse;
pub(crate) use crate::services::subgraph::Request as SubgraphRequest;
pub(crate) use crate::services::subgraph::Response as SubgraphResponse;
pub(crate) use crate::services::supergraph::Request as SupergraphRequest;
pub(crate) use crate::services::supergraph::Response as SupergraphResponse;

pub mod execution;
mod execution_service;
pub(crate) mod layers;
pub(crate) mod new_service;
pub(crate) mod query_planner;
pub mod subgraph;
pub(crate) mod subgraph_service;
pub mod supergraph;
mod supergraph_service;
pub mod transport;

impl AsRef<Request> for http_ext::Request<Request> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}

impl AsRef<Request> for Arc<http_ext::Request<Request>> {
    fn as_ref(&self) -> &Request {
        self.body()
    }
}
