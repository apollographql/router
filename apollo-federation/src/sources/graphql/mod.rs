use crate::error::FederationError;
use crate::query_plan::operation::NormalizedSelectionSet;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::schema::position::{
    AbstractFieldDefinitionPosition, AbstractTypeDefinitionPosition,
    CompositeTypeDefinitionPosition, EnumTypeDefinitionPosition, ObjectFieldDefinitionPosition,
    ObjectOrInterfaceFieldDirectivePosition, ObjectTypeDefinitionPosition,
    ScalarTypeDefinitionPosition, SchemaRootDefinitionKind,
};
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::source_aware::federated_query_graph::graph_path::{
    ConditionResolutionId, OperationPathElement,
};
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTreeChildKey;
use crate::source_aware::federated_query_graph::{FederatedQueryGraph, SelfConditionIndex};
use crate::source_aware::query_plan::{FetchDataPathElement, QueryPlanCost};
use crate::sources::{
    SourceFederatedQueryGraphBuilderApi, SourceFetchDependencyGraphApi,
    SourceFetchDependencyGraphNode, SourceFetchNode, SourceId, SourcePath, SourcePathApi,
};
use crate::ValidFederationSubgraph;
use apollo_compiler::executable::{OperationType, VariableDefinition};
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Node, NodeStr};
use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;
use std::sync::Arc;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct GraphqlId {
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
    fn can_reuse_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _source_data: &SourceFetchDependencyGraphNode,
        _path_tree_edges: Vec<FederatedPathTreeChildKey>,
    ) -> Result<Vec<FederatedPathTreeChildKey>, FederationError> {
        todo!()
    }

    fn add_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<SourceFetchDependencyGraphNode, FederationError> {
        todo!()
    }

    fn new_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<SourcePath, FederationError> {
        todo!()
    }

    fn add_path(
        &self,
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
        selection_set: NormalizedSelectionSet,
    },
    EntitiesField {
        key_condition_resolution: Option<ConditionResolutionId>,
        selection_set: NormalizedSelectionSet,
    },
}

#[derive(Debug)]
pub(crate) struct GraphqlPath {
    query_graph: Arc<FederatedQueryGraph>,
    merge_at: Vec<FetchDataPathElement>,
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

#[derive(Debug)]
pub struct GraphqlFetchNode {
    source_id: GraphqlId,
    operation_document: Valid<ExecutableDocument>,
    operation_name: Option<NodeStr>,
    operation_kind: OperationType,
}
