use std::fmt::Display;
use std::fmt::Formatter;
use apollo_compiler::NodeStr;

use crate::sources::connect::ConnectId;
use crate::sources::graphql::GraphqlId;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
pub mod query_plan;

#[derive(
    Debug, Clone, Copy, Hash, PartialEq, Eq, strum_macros::Display, strum_macros::EnumIter,
)]
pub enum SourceKind {
    #[strum(to_string = "GraphQL")]
    Graphql,
    #[strum(to_string = "Connect")]
    Connect,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, derive_more::From)]
pub enum SourceId {
    Graphql(GraphqlId),
    Connect(ConnectId),
}

impl From<NodeStr> for SourceId {
    fn from(value: NodeStr) -> Self {
        Self::Graphql(value.into())
    }
}

impl SourceId {
    pub(crate) fn kind(&self) -> SourceKind {
        match self {
            SourceId::Graphql(_) => SourceKind::Graphql,
            SourceId::Connect(_) => SourceKind::Connect,
        }
    }
}

impl Display for SourceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let kind = self.kind();
        match self {
            SourceId::Graphql(id) => {
                write!(f, "{kind}:{id}")
            }
            SourceId::Connect(id) => {
                write!(f, "{kind}:{id}")
            }
        }
    }
}
