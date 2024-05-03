use std::sync::Arc;

use indexmap::IndexMap;
use indexmap::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::query_plan::operation::NormalizedField;
use crate::query_plan::operation::NormalizedInlineFragment;
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTree;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;
use crate::source_aware::query_plan::QueryPlanCost;

#[derive(Debug, Clone)]
pub(crate) struct FederatedGraphPath {
    graph: Arc<FederatedQueryGraph>,
    head: NodeIndex,
    tail: NodeIndex,
    edges: Vec<Arc<FederatedGraphPathEdge>>,
    last_source_entering_edge_info: Option<SourceEnteringEdgeInfo>,
    possible_concrete_nodes_at_tail: Arc<IndexSet<NodeIndex>>,
    possible_concrete_nodes_before_last_edge_if_type_condition: Option<Arc<IndexSet<NodeIndex>>>,
}

#[derive(Debug)]
pub(crate) struct FederatedGraphPathEdge {
    operation_element: Option<Arc<OperationPathElement>>,
    edge: Option<EdgeIndex>,
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    source_entering_condition_resolutions_at_edge:
        IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    condition_resolutions_at_edge: IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::From)]
pub(crate) enum OperationPathElement {
    Field(NormalizedField),
    InlineFragment(NormalizedInlineFragment),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct ConditionResolutionId(usize);

#[derive(Debug)]
pub(crate) struct ConditionResolutionInfo {
    id: ConditionResolutionId,
    resolution: Arc<FederatedPathTree>,
    cost: QueryPlanCost,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceEnteringEdgeInfo {
    index: usize,
    conditions_cost: QueryPlanCost,
}
