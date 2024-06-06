use std::sync::Arc;

use apollo_compiler::ast::VariableDefinition;
use apollo_compiler::Node as NodeElement;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;

use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::path_tree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::source;
use crate::sources::source::fetch_dependency_graph::FetchDependencyGraphApi;
use crate::sources::source::fetch_dependency_graph::PathApi;
use crate::sources::source::SourceId;

#[derive(Debug)]
pub(crate) struct FetchDependencyGraph {
    parameters: Arc<QueryPlannerConfig>,
    variable_definitions: Vec<NodeElement<VariableDefinition>>,
    operation_name: Option<NodeStr>,
}

impl FetchDependencyGraphApi for FetchDependencyGraph {
    fn edges_that_can_reuse_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: &[FetchDataPathElement],
        _source_entering_edge: EdgeIndex,
        _path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
        _source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<Vec<&'path_tree path_tree::ChildKey>, FederationError> {
        todo!()
    }

    fn add_node<'path_tree>(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _merge_at: Arc<[FetchDataPathElement]>,
        _source_entering_edge: EdgeIndex,
        _self_condition_resolution: Option<ConditionResolutionId>,
        _path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
    ) -> Result<
        (
            source::fetch_dependency_graph::Node,
            Vec<&'path_tree path_tree::ChildKey>,
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
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        todo!()
    }

    fn add_path(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_path: source::fetch_dependency_graph::Path,
        _source_data: &mut source::fetch_dependency_graph::Node,
    ) -> Result<(), FederationError> {
        todo!()
    }

    fn to_cost(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
    ) -> Result<QueryPlanCost, FederationError> {
        todo!()
    }

    fn to_plan_node(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _source_id: SourceId,
        _source_data: &source::fetch_dependency_graph::Node,
        _fetch_count: u32,
    ) -> Result<source::query_plan::FetchNode, FederationError> {
        todo!()
    }
}

#[derive(Debug)]
pub(crate) enum Node {
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
pub(crate) struct Path {
    merge_at: Arc<[FetchDataPathElement]>,
    source_entering_edge: EdgeIndex,
    source_id: SourceId,
    operation_path: Vec<OperationPathElement>,
}

impl PathApi for Path {
    fn source_id(&self) -> &SourceId {
        todo!()
    }

    fn add_operation_element(
        &self,
        _query_graph: Arc<FederatedQueryGraph>,
        _operation_element: Arc<OperationPathElement>,
        _edge: Option<EdgeIndex>,
        _self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<source::fetch_dependency_graph::Path, FederationError> {
        todo!()
    }
}
