use crate::error::FederationError;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::{FederatedQueryGraph, SelfConditionIndex};
use crate::sources::{
    SourceFederatedAbstractFieldQueryGraphEdge, SourceFederatedConcreteFieldQueryGraphEdge,
    SourceFederatedConcreteQueryGraphNode, SourceFederatedEnumQueryGraphNode,
    SourceFederatedQueryGraphBuilders, SourceFederatedQueryGraphs,
    SourceFederatedScalarQueryGraphNode, SourceFederatedTypeConditionQueryGraphEdge, SourceId,
};
use crate::ValidFederationSubgraph;
use apollo_compiler::schema::{Name, NamedType};
use apollo_compiler::NodeStr;
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::{EdgeIndex, NodeIndex};

struct FederatedQueryGraphBuilder {
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
    for_query_planning: bool,
    source_data: SourceFederatedQueryGraphBuilders,
}

impl FederatedQueryGraphBuilder {
    pub(crate) fn new(
        _supergraph_schema: ValidFederationSchema,
        _api_schema: ValidFederationSchema,
        _for_query_planning: bool,
    ) -> Result<Self, FederationError> {
        todo!()
    }

    pub(crate) fn build(self) -> Result<FederatedQueryGraph, FederationError> {
        todo!()
    }
}

struct IntraSourceQueryGraphBuilder {
    graph: FederatedQueryGraph,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    for_query_planning: bool,
}

pub(crate) trait IntraSourceQueryGraphBuilderApi {
    fn source_query_graph(&mut self) -> &mut SourceFederatedQueryGraphs;

    fn is_for_query_planning(&self) -> bool;

    fn add_and_set_current_source(&mut self, source: SourceId) -> Result<(), FederationError>;

    fn get_current_source(&self) -> Result<SourceId, FederationError>;

    fn add_self_condition(
        &mut self,
        supergraph_type_name: NamedType,
        field_set: &str,
    ) -> Result<SelfConditionIndex, FederationError>;

    fn add_abstract_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: SourceFederatedAbstractFieldQueryGraphEdge,
    ) -> Result<NodeIndex, FederationError>;

    fn add_concrete_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: SourceFederatedConcreteQueryGraphNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_enum_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: SourceFederatedEnumQueryGraphNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_scalar_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: SourceFederatedScalarQueryGraphNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_abstract_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: SourceFederatedAbstractFieldQueryGraphEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_concrete_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: SourceFederatedConcreteFieldQueryGraphEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_type_condition_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        source_data: SourceFederatedTypeConditionQueryGraphEdge,
    ) -> Result<EdgeIndex, FederationError>;
}
