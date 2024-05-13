use std::sync::Arc;

use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionInfo;
use crate::source_aware::federated_query_graph::graph_path::OperationPathElement;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::federated_query_graph::SelfConditionIndex;

#[derive(Debug)]
pub(crate) struct FederatedPathTree {
    graph: Arc<FederatedQueryGraph>,
    node: NodeIndex,
    source_entering_condition_resolutions_at_node:
        IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    condition_resolutions_at_node: IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    childs: Vec<Arc<Child>>,
}

#[derive(Debug)]
pub(crate) struct Child {
    key: ChildKey,
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    tree: Arc<FederatedPathTree>,
}

#[derive(Debug)]
pub(crate) struct ChildKey {
    operation_element: Option<Arc<OperationPathElement>>,
    edge: Option<EdgeIndex>,
}
