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
    /// Conditions are imposed by query graph edges and resolved at earlier query graph nodes. When
    /// resolution happens at this node for some later source-entering edge's condition during
    /// option generation, that resolution's information (including ID) is stored in the graph
    /// path's edge's `source_entering_condition_resolutions_at_head`, which gets merged into this
    /// map during path tree generation.
    source_entering_condition_resolutions_at_node:
        IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    /// Conditions are imposed by query graph edges and resolved at earlier query graph nodes. When
    /// resolution happens at this node for some later non-source-entering edge's condition during
    /// option generation, that resolution's information (including ID) is stored in the graph
    /// path's edge's `condition_resolutions_at_head`, which gets merged into this map during path
    /// tree generation.
    condition_resolutions_at_node: IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    childs: Vec<Arc<Child>>,
}

#[derive(Debug)]
pub(crate) struct Child {
    key: ChildKey,
    /// Conditions are imposed by query graph edges and resolved at earlier query graph nodes. When
    /// resolution happens for this edge's condition during option generation, that resolution's ID
    /// is stored in the graph path's edge's `self_condition_resolutions_for_edge`, which gets
    /// merged into this map during path tree generation.
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    tree: Arc<FederatedPathTree>,
}

#[derive(Debug)]
pub(crate) struct ChildKey {
    pub(crate) operation_element: Option<Arc<OperationPathElement>>,
    pub(crate) edge: Option<EdgeIndex>,
}
