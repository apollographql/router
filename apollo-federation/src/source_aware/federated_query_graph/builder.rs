use apollo_compiler::schema::Name;
use apollo_compiler::schema::NamedType;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::sources::source;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;
use crate::ValidFederationSubgraph;

struct FederatedQueryGraphBuilder {
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
    subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
    is_for_query_planning: bool,
    source_data: source::federated_query_graph::builder::FederatedQueryGraphBuilders,
}

impl FederatedQueryGraphBuilder {
    pub(crate) fn new(
        _supergraph_schema: ValidFederationSchema,
        _api_schema: ValidFederationSchema,
        _is_for_query_planning: bool,
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
    is_for_query_planning: bool,
    non_entity_supergraph_types_to_nodes:
        IndexMap<ObjectTypeDefinitionPosition, IndexSet<NodeIndex>>,
    current_source_kind: Option<SourceKind>,
    current_source_id: Option<SourceId>,
}

pub(crate) trait IntraSourceQueryGraphBuilderApi {
    fn source_query_graph(
        &mut self,
    ) -> Result<&mut source::federated_query_graph::FederatedQueryGraph, FederationError>;

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
        source_data: source::federated_query_graph::AbstractNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_concrete_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ConcreteNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_enum_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::EnumNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_scalar_node(
        &mut self,
        supergraph_type_name: NamedType,
        source_data: source::federated_query_graph::ScalarNode,
    ) -> Result<NodeIndex, FederationError>;

    fn add_abstract_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::AbstractFieldEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_concrete_field_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        supergraph_field_name: Name,
        self_conditions: IndexSet<SelfConditionIndex>,
        source_data: source::federated_query_graph::ConcreteFieldEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_type_condition_edge(
        &mut self,
        head: NodeIndex,
        tail: NodeIndex,
        source_data: source::federated_query_graph::TypeConditionEdge,
    ) -> Result<EdgeIndex, FederationError>;

    fn add_source_entering_edge(
        &mut self,
        tail: NodeIndex,
        self_conditions: Option<SelfConditionIndex>,
        source_data: source::federated_query_graph::SourceEnteringEdge,
    ) -> Result<EdgeIndex, FederationError>;
}
