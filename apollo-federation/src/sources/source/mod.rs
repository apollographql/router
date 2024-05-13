use crate::sources::connect::ConnectId;
use crate::sources::graphql::GraphqlId;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
pub mod query_plan;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum SourceKind {
    Graphql,
    Connect,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum SourceId {
    Graphql(GraphqlId),
    Connect(ConnectId),
}

impl SourceId {
    fn kind(&self) -> SourceKind {
        todo!()
    }
}
