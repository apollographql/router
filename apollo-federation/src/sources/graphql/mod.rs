use std::sync::Arc;

use apollo_compiler::executable::OperationType;
use apollo_compiler::executable::VariableDefinition;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;

use crate::error::FederationError;
use crate::query_plan::operation::SelectionSet;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::schema::position::AbstractFieldDefinitionPosition;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTreeChildKey;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::SourceFederatedQueryGraphBuilderApi;
use crate::sources::SourceFetchDependencyGraphApi;
use crate::sources::SourceFetchDependencyGraphNode;
use crate::sources::SourceFetchNode;
use crate::sources::SourceId;
use crate::sources::SourcePath;
use crate::sources::SourcePathApi;
use crate::ValidFederationSubgraph;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct GraphqlId {
    subgraph_name: NodeStr,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedQueryGraph {
    subgraphs_by_source: IndexMap<GraphqlId, ValidFederationSubgraph>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedAbstractQueryGraphNode {
    subgraph_type: AbstractTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedConcreteQueryGraphNode {
    subgraph_type: ObjectTypeDefinitionPosition,
    provides_directive: Option<ObjectOrInterfaceFieldDirectivePosition>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedEnumQueryGraphNode {
    subgraph_type: EnumTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedScalarQueryGraphNode {
    subgraph_type: ScalarTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedAbstractFieldQueryGraphEdge {
    subgraph_field: AbstractFieldDefinitionPosition,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedConcreteFieldQueryGraphEdge {
    subgraph_field: ObjectFieldDefinitionPosition,
    requires_condition: Option<SelfConditionIndex>,
}

#[derive(Debug)]
pub(crate) struct GraphqlFederatedTypeConditionQueryGraphEdge {
    subgraph_type: CompositeTypeDefinitionPosition,
}

#[derive(Debug)]
pub(crate) enum GraphqlFederatedSourceEnteringQueryGraphEdge {
    OperationRoot {
        subgraph_type: ObjectTypeDefinitionPosition,
        root_kind: SchemaRootDefinitionKind,
    },
    EntitiesField {
        subgraph_type: ObjectTypeDefinitionPosition,
        key_condition: Option<SelfConditionIndex>,
    },
}

pub(crate) struct GraphqlFederatedQueryGraphBuilder;

impl SourceFederatedQueryGraphBuilderApi for GraphqlFederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        _subgraph: ValidFederationSubgraph,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) struct GraphqlFetchDependencyGraph {
    parameters: Arc<QueryPlannerConfig>,
    variable_definitions: Vec<Node<VariableDefinition>>,
    operation_name: Option<NodeStr>,
}

impl SourceFetchDependencyGraphApi for GraphqlFetchDependencyGraph {
    fn can_reuse_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
        _source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<Vec<&'path_tree FederatedPathTreeChildKey>, FederationError> {
        todo!()
    }

    fn add_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<[FetchDataPathElement]>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
        _path_tree_edges: Vec<&'path_tree FederatedPathTreeChildKey>,
    ) -> Result<
        (
            SourceFetchDependencyGraphNode,
            Vec<&'path_tree FederatedPathTreeChildKey>,
        ),
        FederationError,
    > {
        todo!()
    }

    fn new_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<[FetchDataPathElement]>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError> {
        todo!()
    }

    fn add_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_path: SourcePath,
        _source_data: &mut SourceFetchDependencyGraphNode,
    ) -> Result<(), FederationError> {
        todo!()
    }

    fn to_cost(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &SourceFetchDependencyGraphNode,
    ) -> Result<QueryPlanCost, FederationError> {
        todo!()
    }

    fn to_plan_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &SourceFetchDependencyGraphNode,
        _fetch_count: u32,
    ) -> Result<SourceFetchNode, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) enum GraphqlFetchDependencyGraphNode {
    OperationRoot {
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        selection_set: SelectionSet,
    },
    EntitiesField {
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        key_condition_resolution: Option<ConditionResolutionId>,
        selection_set: SelectionSet,
    },
}

#[derive(Debug)]
pub(crate) struct GraphqlPath {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    operation_path: Vec<OperationPathElement>,
}

impl SourcePathApi for GraphqlPath {
    fn source_id(&self) -> &SourceId {
        todo!()
    }

    fn add_operation_element(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _operation_element: Arc<OperationPathElement>,
        _edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError> {
        todo!()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphqlFetchNode {
    pub source_id: GraphqlId,
    pub operation_document: Valid<ExecutableDocument>,
    pub operation_name: Option<NodeStr>,
    pub operation_kind: OperationType,
}
