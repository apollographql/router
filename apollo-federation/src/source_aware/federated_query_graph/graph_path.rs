use std::sync::Arc;

use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::query_plan::operation::Field;
use crate::query_plan::operation::InlineFragment;
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::QueryPlanCost;

#[derive(Debug, Clone)]
pub(crate) struct FederatedGraphPath {
    graph: Arc<FederatedQueryGraph>,
    head: NodeIndex,
    tail: NodeIndex,
    edges: Vec<Arc<Edge>>,
    last_source_entering_edge_info: Option<SourceEnteringEdgeInfo>,
    possible_concrete_nodes_at_tail: Arc<IndexSet<NodeIndex>>,
    possible_concrete_nodes_before_last_edge_if_type_condition: Option<Arc<IndexSet<NodeIndex>>>,
}

#[derive(Debug)]
pub(crate) struct Edge {
    pub operation_element: Option<Arc<OperationPathElement>>,
    pub edge: Option<EdgeIndex>,
    /// Conditions imposed by the edge (eg. @key on a source entering edge or @requires on a field edge)
    pub self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    pub source_entering_condition_resolutions_at_head:
        IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    pub condition_resolutions_at_head: IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::From)]
pub(crate) enum OperationPathElement {
    Field(Field),
    InlineFragment(InlineFragment),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) struct ConditionResolutionId(usize);

#[derive(Debug, Clone)] // TODO(@goto-bus-stop): should this be cloneable?
pub(crate) struct ConditionResolutionInfo {
    pub id: ConditionResolutionId,
    pub resolution: Arc<FederatedPathTree>,
    pub cost: QueryPlanCost,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceEnteringEdgeInfo {
    index: usize,
    conditions_cost: QueryPlanCost,
}

impl FederatedGraphPath {
    pub fn edges(&self) -> impl Iterator<Item = &'_ Edge> {
        self.edges.iter().map(|arc| &**arc)
    }
}
