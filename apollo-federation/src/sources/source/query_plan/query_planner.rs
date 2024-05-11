use crate::sources::connect;
use crate::sources::graphql;

#[derive(Debug, derive_more::From)]
pub enum ExecutionMetadata {
    Graphql(graphql::query_plan::query_planner::ExecutionMetadata),
    Connect(connect::query_plan::query_planner::ExecutionMetadata),
}
