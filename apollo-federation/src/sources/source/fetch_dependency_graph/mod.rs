use std::sync::Arc;

use enum_dispatch::enum_dispatch;
use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;

use crate::error::FederationError;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::path_tree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::FetchDataPathElement;
use crate::source_aware::query_plan::QueryPlanCost;
use crate::sources::connect;
use crate::sources::graphql;
use crate::sources::source::query_plan::FetchNode;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;

#[derive(Debug)]
#[enum_dispatch(FetchDependencyGraphApi)]
pub(crate) enum FetchDependencyGraph {
    Graphql(graphql::fetch_dependency_graph::FetchDependencyGraph),
    Connect(connect::fetch_dependency_graph::FetchDependencyGraph),
}

#[enum_dispatch]
pub(crate) trait FetchDependencyGraphApi {
    fn edges_that_can_reuse_node<'path_tree>(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: &[FetchDataPathElement],
        source_entering_edge: EdgeIndex,
        path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
        source_data: &Node,
    ) -> Result<Vec<&'path_tree path_tree::ChildKey>, FederationError>;

    fn add_node<'path_tree>(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        self_condition_resolution: Option<ConditionResolutionId>,
        path_tree_edges: Vec<&'path_tree path_tree::ChildKey>,
    ) -> Result<(Node, Vec<&'path_tree path_tree::ChildKey>), FederationError>;

    fn new_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        merge_at: Arc<[FetchDataPathElement]>,
        source_entering_edge: EdgeIndex,
        self_condition_resolution: Option<ConditionResolutionId>,
    ) -> Result<Path, FederationError>;

    fn add_path(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        source_path: Path,
        source_data: &mut Node,
    ) -> Result<(), FederationError>;

    fn to_cost(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        source_id: SourceId,
        source_data: &Node,
    ) -> Result<QueryPlanCost, FederationError>;

    fn to_plan_node(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        source_id: SourceId,
        source_data: &Node,
        fetch_count: u32,
    ) -> Result<FetchNode, FederationError>;
}

#[derive(Debug)]
pub(crate) struct FetchDependencyGraphs {
    builders: IndexMap<SourceKind, FetchDependencyGraph>,
}

#[derive(Debug, derive_more::From)]
pub(crate) enum Node {
    Graphql(graphql::fetch_dependency_graph::Node),
    Connect(connect::fetch_dependency_graph::Node),
}

#[derive(Debug)]
#[enum_dispatch(PathApi)]
pub(crate) enum Path {
    Graphql(graphql::fetch_dependency_graph::Path),
    Connect(connect::fetch_dependency_graph::Path),
}

#[enum_dispatch]
pub(crate) trait PathApi {
    fn source_id(&self) -> &SourceId;

    fn add_operation_element(
        &self,
        query_graph: Arc<FederatedQueryGraph>,
        operation_element: Arc<OperationPathElement>,
        edge: Option<EdgeIndex>,
        self_condition_resolutions: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    ) -> Result<Path, FederationError>;
}
