use enum_dispatch::enum_dispatch;
use indexmap::IndexMap;

use crate::sources::connect;
use crate::sources::graphql;
use crate::sources::source::query_plan::query_planner::ExecutionMetadata;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;

pub(crate) mod builder;

#[derive(Debug)]
#[enum_dispatch(FederatedQueryGraphApi)]
pub(crate) enum FederatedQueryGraph {
    Graphql(graphql::federated_query_graph::FederatedQueryGraph),
    Connect(connect::federated_query_graph::FederatedQueryGraph),
}

#[enum_dispatch]
pub(crate) trait FederatedQueryGraphApi {
    fn execution_metadata(&self) -> IndexMap<SourceId, ExecutionMetadata>;
}

#[derive(Debug)]
pub(crate) struct FederatedQueryGraphs {
    pub(crate) graphs: IndexMap<SourceKind, FederatedQueryGraph>,
}

impl Default for FederatedQueryGraphs {
    fn default() -> Self {
        Self {
            graphs: IndexMap::from([
                (
                    SourceKind::Graphql,
                    FederatedQueryGraph::Graphql(Default::default()),
                ),
                (
                    SourceKind::Connect,
                    FederatedQueryGraph::Connect(Default::default()),
                ),
            ]),
        }
    }
}

#[cfg(test)]
impl FederatedQueryGraphs {
    pub(crate) fn with_graphs(graphs: IndexMap<SourceKind, FederatedQueryGraph>) -> Self {
        Self { graphs }
    }
}

#[derive(Debug, derive_more::From)]
pub(crate) enum AbstractNode {
    Graphql(graphql::federated_query_graph::AbstractNode),
    Connect(connect::federated_query_graph::AbstractNode),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum ConcreteNode {
    Graphql(graphql::federated_query_graph::ConcreteNode),
    Connect(connect::federated_query_graph::ConcreteNode),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum EnumNode {
    Graphql(graphql::federated_query_graph::EnumNode),
    Connect(connect::federated_query_graph::EnumNode),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum ScalarNode {
    Graphql(graphql::federated_query_graph::ScalarNode),
    Connect(connect::federated_query_graph::ScalarNode),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum AbstractFieldEdge {
    Graphql(graphql::federated_query_graph::AbstractFieldEdge),
    Connect(connect::federated_query_graph::AbstractFieldEdge),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum ConcreteFieldEdge {
    Graphql(graphql::federated_query_graph::ConcreteFieldEdge),
    Connect(connect::federated_query_graph::ConcreteFieldEdge),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum TypeConditionEdge {
    Graphql(graphql::federated_query_graph::TypeConditionEdge),
    Connect(connect::federated_query_graph::TypeConditionEdge),
}

#[derive(Debug, derive_more::From)]
pub(crate) enum SourceEnteringEdge {
    Graphql(graphql::federated_query_graph::SourceEnteringEdge),
    Connect(connect::federated_query_graph::SourceEnteringEdge),
}
