use crate::error::FederationError;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::sources::connect::{
    ConnectFederatedAbstractFieldQueryGraphEdge, ConnectFederatedAbstractQueryGraphNode,
    ConnectFederatedConcreteFieldQueryGraphEdge, ConnectFederatedConcreteQueryGraphNode,
    ConnectFederatedEnumQueryGraphNode, ConnectFederatedLookupQueryGraphEdge,
    ConnectFederatedQueryGraph, ConnectFederatedQueryGraphBuilder,
    ConnectFederatedScalarQueryGraphNode, ConnectFederatedTypeConditionQueryGraphEdge,
    ConnectFetchNode, ConnectId,
};
use crate::sources::graphql::{
    GraphqlFederatedAbstractFieldQueryGraphEdge, GraphqlFederatedAbstractQueryGraphNode,
    GraphqlFederatedConcreteFieldQueryGraphEdge, GraphqlFederatedConcreteQueryGraphNode,
    GraphqlFederatedEnumQueryGraphNode, GraphqlFederatedLookupQueryGraphEdge,
    GraphqlFederatedQueryGraph, GraphqlFederatedQueryGraphBuilder,
    GraphqlFederatedScalarQueryGraphNode, GraphqlFederatedTypeConditionQueryGraphEdge,
    GraphqlFetchNode, GraphqlId,
};
use crate::ValidFederationSubgraph;
use apollo_compiler::NodeStr;
use enum_dispatch::enum_dispatch;
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::NodeIndex;

pub mod connect;
pub mod graphql;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Root,
    Graphql,
    Connect,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum SourceId {
    Root,
    Graphql(GraphqlId),
    Connect(ConnectId),
}

impl SourceId {
    fn kind(&self) -> SourceKind {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct SourceFederatedQueryGraphs {
    graphql: GraphqlFederatedQueryGraph,
    connect: ConnectFederatedQueryGraph,
}

#[derive(Debug)]
pub(crate) enum SourceFederatedAbstractQueryGraphNode {
    Graphql(GraphqlFederatedAbstractQueryGraphNode),
    Connect(ConnectFederatedAbstractQueryGraphNode),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedConcreteQueryGraphNode {
    Root,
    Graphql(GraphqlFederatedConcreteQueryGraphNode),
    Connect(ConnectFederatedConcreteQueryGraphNode),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedEnumQueryGraphNode {
    Graphql(GraphqlFederatedEnumQueryGraphNode),
    Connect(ConnectFederatedEnumQueryGraphNode),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedScalarQueryGraphNode {
    Graphql(GraphqlFederatedScalarQueryGraphNode),
    Connect(ConnectFederatedScalarQueryGraphNode),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedAbstractFieldQueryGraphEdge {
    Graphql(GraphqlFederatedAbstractFieldQueryGraphEdge),
    Connect(ConnectFederatedAbstractFieldQueryGraphEdge),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedConcreteFieldQueryGraphEdge {
    Graphql(GraphqlFederatedConcreteFieldQueryGraphEdge),
    Connect(ConnectFederatedConcreteFieldQueryGraphEdge),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedTypeConditionQueryGraphEdge {
    Graphql(GraphqlFederatedTypeConditionQueryGraphEdge),
    Connect(ConnectFederatedTypeConditionQueryGraphEdge),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedLookupQueryGraphEdge {
    Graphql(GraphqlFederatedLookupQueryGraphEdge),
    Connect(ConnectFederatedLookupQueryGraphEdge),
}

#[enum_dispatch(SourceFederatedQueryGraphBuilderApi)]
pub(crate) enum SourceFederatedQueryGraphBuilder {
    Graphql(GraphqlFederatedQueryGraphBuilder),
    Connect(ConnectFederatedQueryGraphBuilder),
}

#[enum_dispatch]
pub(crate) trait SourceFederatedQueryGraphBuilderApi {
    fn process_subgraph_schema(
        &self,
        subgraph: ValidFederationSubgraph,
        builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<Vec<FederatedLookupTailData>, FederationError>;
}

pub(crate) struct FederatedLookupTailData {
    tail: NodeIndex,
    self_conditions: IndexSet<SelfConditionIndex>,
    source_data: SourceFederatedLookupQueryGraphEdge,
}

pub(crate) struct SourceFederatedQueryGraphBuilders {
    builders: IndexMap<SourceKind, SourceFederatedQueryGraphBuilder>,
}

impl SourceFederatedQueryGraphBuilders {
    fn new() -> Self {
        todo!()
    }

    fn process_subgraph_schemas(
        _subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<Vec<FederatedLookupTailData>, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub enum SourceFetchNode {
    Graphql(GraphqlFetchNode),
    Connect(ConnectFetchNode),
}
