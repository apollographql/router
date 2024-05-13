use apollo_compiler::NodeStr;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
pub mod query_plan;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct GraphqlId {
    subgraph_name: NodeStr,
}
