use crate::source_aware::federated_query_graph::graph_path::{
    ConditionResolutionId, ConditionResolutionInfo, OperationPathElement,
};
use crate::source_aware::federated_query_graph::{FederatedQueryGraph, SelfConditionIndex};
use indexmap::IndexMap;
use petgraph::graph::{EdgeIndex, NodeIndex};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct FederatedPathTree {
    graph: Arc<FederatedQueryGraph>,
    node: NodeIndex,
    childs: Vec<Arc<FederatedPathTreeChild>>,
}

#[derive(Debug)]
pub(crate) struct FederatedPathTreeChild {
    key: FederatedPathTreeChildKey,
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    source_entering_condition_resolutions_at_edge:
        IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    condition_resolutions_at_edge: IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    tree: Arc<FederatedPathTree>,
}

#[derive(Debug)]
pub(crate) struct FederatedPathTreeChildKey {
    operation_element: Option<Arc<OperationPathElement>>,
    edge: Option<EdgeIndex>,
}
