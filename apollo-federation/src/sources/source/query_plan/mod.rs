use crate::sources::connect;
use crate::sources::graphql;

pub mod query_planner;

#[derive(Debug, Clone, PartialEq, derive_more::From)]
pub enum FetchNode {
    Graphql(graphql::query_plan::FetchNode),
    Connect(connect::query_plan::FetchNode),
}
