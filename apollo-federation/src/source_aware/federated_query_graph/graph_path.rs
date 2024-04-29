use crate::query_plan::operation::normalized_field_selection::NormalizedField;
use crate::query_plan::operation::normalized_inline_fragment_selection::NormalizedInlineFragment;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::source_aware::federated_query_graph::path_tree::FederatedPathTree;
use crate::source_aware::federated_query_graph::{FederatedQueryGraph, SelfConditionIndex};
use crate::source_aware::query_plan::QueryPlanCost;
use indexmap::{IndexMap, IndexSet};
use petgraph::graph::{EdgeIndex, NodeIndex};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct FederatedGraphPath {
    graph: Arc<FederatedQueryGraph>,
    head: NodeIndex,
    tail: NodeIndex,
    edges: Vec<Arc<FederatedGraphPathEdge>>,
    last_source_enter_edge_info: Option<SourceEnterEdgeInfo>,
    runtime_types_at_tail: Arc<IndexSet<ObjectTypeDefinitionPosition>>,
    runtime_types_before_last_edge_if_type_condition:
        Option<Arc<IndexSet<ObjectTypeDefinitionPosition>>>,
}

#[derive(Debug)]
pub(crate) struct FederatedGraphPathEdge {
    operation_element: Option<Arc<OperationPathElement>>,
    edge: Option<EdgeIndex>,
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    source_enter_condition_resolutions_at_edge:
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
pub(crate) struct SourceEnterEdgeInfo {
    index: usize,
    conditions_cost: QueryPlanCost,
}
