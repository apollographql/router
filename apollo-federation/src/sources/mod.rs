use crate::error::FederationError;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::graph_path::{
    ConditionResolutionId, OperationPathElement,
};
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTreeChildKey;
use crate::source_aware::federated_query_graph::{FederatedQueryGraph, SelfConditionIndex};
use crate::source_aware::query_plan::{FetchDataPathElement, QueryPlanCost};
use crate::sources::connect::{
    ConnectFederatedAbstractFieldQueryGraphEdge, ConnectFederatedAbstractQueryGraphNode,
    ConnectFederatedConcreteFieldQueryGraphEdge, ConnectFederatedConcreteQueryGraphNode,
    ConnectFederatedEnumQueryGraphNode, ConnectFederatedQueryGraph,
    ConnectFederatedQueryGraphBuilder, ConnectFederatedScalarQueryGraphNode,
    ConnectFederatedSourceEnteringQueryGraphEdge, ConnectFederatedTypeConditionQueryGraphEdge,
    ConnectFetchDependencyGraph, ConnectFetchDependencyGraphNode, ConnectFetchNode, ConnectId,
    ConnectPath,
};
use crate::sources::graphql::{
    GraphqlFederatedAbstractFieldQueryGraphEdge, GraphqlFederatedAbstractQueryGraphNode,
    GraphqlFederatedConcreteFieldQueryGraphEdge, GraphqlFederatedConcreteQueryGraphNode,
    GraphqlFederatedEnumQueryGraphNode, GraphqlFederatedQueryGraph,
    GraphqlFederatedQueryGraphBuilder, GraphqlFederatedScalarQueryGraphNode,
    GraphqlFederatedSourceEnteringQueryGraphEdge, GraphqlFederatedTypeConditionQueryGraphEdge,
    GraphqlFetchDependencyGraph, GraphqlFetchDependencyGraphNode, GraphqlFetchNode, GraphqlId,
    GraphqlPath,
};
use crate::ValidFederationSubgraph;
use apollo_compiler::NodeStr;
use enum_dispatch::enum_dispatch;
use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;
use std::sync::Arc;

pub mod connect;
pub mod graphql;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Graphql,
    Connect,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum SourceId {
    Graphql(GraphqlId),
    Connect(ConnectId),
}

impl SourceId {
    fn kind(&self) -> SourceKind {
        todo!()
    }
}

pub(crate) enum SourceFederatedQueryGraph {
    Graphql(GraphqlFederatedQueryGraph),
    Connect(ConnectFederatedQueryGraph),
}

#[derive(Debug)]
pub(crate) struct SourceFederatedQueryGraphs {
    graphs: IndexMap<SourceKind, SourceFetchDependencyGraph>,
}

#[derive(Debug)]
pub(crate) enum SourceFederatedAbstractQueryGraphNode {
    Graphql(GraphqlFederatedAbstractQueryGraphNode),
    Connect(ConnectFederatedAbstractQueryGraphNode),
}

#[derive(Debug)]
pub(crate) enum SourceFederatedConcreteQueryGraphNode {
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
pub(crate) enum SourceFederatedSourceEnteringQueryGraphEdge {
    Graphql(GraphqlFederatedSourceEnteringQueryGraphEdge),
    Connect(ConnectFederatedSourceEnteringQueryGraphEdge),
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
    ) -> Result<(), FederationError>;
}

pub(crate) struct SourceFederatedQueryGraphBuilders {
    builders: IndexMap<SourceKind, SourceFederatedQueryGraphBuilder>,
}

impl SourceFederatedQueryGraphBuilders {
    fn new() -> Self {
        todo!()
    }

    fn process_subgraph_schemas(
        &self,
        _subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        todo!()
    }
}

#[derive(Debug)]
#[enum_dispatch(SourceFetchDependencyGraphApi)]
pub(crate) enum SourceFetchDependencyGraph {
    Graphql(GraphqlFetchDependencyGraph),
    Connect(ConnectFetchDependencyGraph),
}

#[enum_dispatch]
pub(crate) trait SourceFetchDependencyGraphApi {
    fn can_reuse_node<'path_tree>(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: &[FetchDataPathElement],
        source_entering_edge: EdgeIndex,
        path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
        source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<Vec<&'path_tree FederatedPathTreeChildKey>, FederationError>;

    fn add_node<'path_tree>(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: &[FetchDataPathElement],
        source_entering_edge: EdgeIndex,
        self_condition_resolution: Option<ConditionResolutionId>,
        path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
    ) -> Result<
        (
            SourceFetchDependencyGraphNode,
            Vec<&'path_tree FederatedPathTreeChildKey>,
        ),
        FederationError,
    >;

    fn new_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: &[FetchDataPathElement],
        source_entering_edge: EdgeIndex,
        self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError>;

    fn add_path(
        &self,
        source_path: SourcePath,
        source_data: &mut SourceFetchDependencyGraphNode,
    ) -> Result<(), FederationError>;

    fn to_cost(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        source_id: SourceId,
        source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<QueryPlanCost, FederationError>;

    fn to_plan_node(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        source_id: SourceId,
        source_data: &SourceFetchDependencyGraphNode,
        fetch_count: u32,
    ) -> Result<SourceFetchNode, FederationError>;
}

#[derive(Debug)]
pub(crate) struct SourceFetchDependencyGraphs {
    builders: IndexMap<SourceKind, SourceFetchDependencyGraph>,
}

#[derive(Debug)]
pub(crate) enum SourceFetchDependencyGraphNode {
    Graphql(GraphqlFetchDependencyGraphNode),
    Connect(ConnectFetchDependencyGraphNode),
}

#[derive(Debug)]
#[enum_dispatch(SourcePathApi)]
pub(crate) enum SourcePath {
    Graphql(GraphqlPath),
    Connect(ConnectPath),
}

#[enum_dispatch]
pub(crate) trait SourcePathApi {
    fn source_id(&self) -> &SourceId;

    fn add_operation_element(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        operation_element: Arc<OperationPathElement>,
        edge: Option<EdgeIndex>,
        self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError>;
}

#[derive(Debug)]
pub enum SourceFetchNode {
    Graphql(GraphqlFetchNode),
    Connect(ConnectFetchNode),
}
