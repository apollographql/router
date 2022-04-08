//! Utilities which make it easy to work with [`crate::plugin`].

pub mod test;

pub mod structures;
pub use structures::{
    execution_request::ExecutionRequest, execution_response::ExecutionResponse,
    queryplanner_response::QueryPlannerResponse, router_request::RouterRequest,
    router_response::RouterResponse, subgraph_request::SubgraphRequest,
    subgraph_response::SubgraphResponse,
};
