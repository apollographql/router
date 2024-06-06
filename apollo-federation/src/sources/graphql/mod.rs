use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::NodeStr;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
pub mod query_plan;

#[derive(Debug, Clone, Hash, PartialEq, Eq, derive_more::From)]
pub struct GraphqlId {
    subgraph_name: NodeStr,
}

impl Display for GraphqlId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.subgraph_name)
    }
}
