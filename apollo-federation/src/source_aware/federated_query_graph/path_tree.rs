use std::sync::Arc;

use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::query_plan::operation::SelectionSet;
use crate::source_aware::federated_query_graph::graph_path;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionId;
use crate::source_aware::federated_query_graph::graph_path::ConditionResolutionInfo;
use crate::source_aware::federated_query_graph::graph_path::FederatedGraphPath;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildKey {
    pub(crate) operation_element: Option<Arc<OperationPathElement>>,
    pub(crate) edge: Option<EdgeIndex>,
}

impl FederatedPathTree {
    // This is not generic in practice: the EdgeIter and PathIter types are just not
    // nameable.
    fn from_paths_inner<'a, EdgeIter, PathIter>(
        graph: Arc<FederatedQueryGraph>,
        node: NodeIndex,
        paths: PathIter,
    ) -> Result<Self, FederationError>
    where
        EdgeIter: Iterator<Item = &'a graph_path::Edge>,
        PathIter: Iterator<Item = (EdgeIter, Option<Arc<SelectionSet>>)>,
    {
        let mut condition_resolutions_at_node = Default::default();
        let mut source_entering_condition_resolutions_at_node = Default::default();
        let mut childs = Default::default();

        struct ByUniqueEdge<'a, EdgeIter>
        where
            EdgeIter: Iterator<Item = &'a graph_path::Edge>,
        {
            target_node: NodeIndex,
            by_unique_trigger:
                IndexMap<Option<Arc<OperationPathElement>>, PathTreeChildInputs<'a, EdgeIter>>,
        }

        struct PathTreeChildInputs<'a, EdgeIter>
        where
            EdgeIter: Iterator<Item = &'a graph_path::Edge>,
        {
            self_condition_resolutions_for_edge:
                IndexMap<SelfConditionIndex, ConditionResolutionId>,
            sub_paths_and_selections: Vec<(EdgeIter, Option<Arc<SelectionSet>>)>,
        }

        let mut merged = IndexMap::new();
        for (graph_path_iter, selection) in paths {
            let Some(edge) = graph_path_iter.next() else {
                continue;
            };
            // Marginally inefficient: we look up edge endpoints even if the edge already exists.
            // This is to make the ? error propagate.
            let for_edge = merged.entry(edge.edge.clone()).or_insert(ByUniqueEdge {
                target_node: if let Some(edge) = &edge.edge {
                    let (_source, target) = graph.edge_endpoints(*edge)?;
                    target
                } else {
                    // For a "None" edge, stay on the same node
                    node
                },
                by_unique_trigger: IndexMap::new(),
            });

            for_edge
                .by_unique_trigger
                .entry(edge.operation_element.clone())
                .and_modify(|existing| {
                    existing.self_condition_resolutions_for_edge = merge_conditions(
                        &existing.self_condition_resolutions_for_edge,
                        self_condition_resolutions_for_edge,
                    );
                    existing
                        .sub_paths_and_selections
                        .push((graph_path_iter, selection))
                    // Note that as we merge, we don't create a new child
                })
                .or_insert_with(|| PathTreeChildInputs {
                    self_condition_resolutions_for_edge: self_condition_resolutions_for_edge
                        .clone(),
                    sub_paths_and_selections: vec![(graph_path_iter, selection)],
                });
        }

        let mut childs = vec![];
        for (edge, by_unique_edge) in merged {
            for (operation_element, child) in by_unique_edge.by_unique_trigger {
                childs.push(Arc::new(Child {
                    key: ChildKey {
                        operation_element,
                        edge,
                    },
                    self_condition_resolutions_for_edge: child.conditions.clone(),
                    tree: Self::from_paths_inner(
                        graph.clone(),
                        by_unique_edge.target_node,
                        child.sub_paths_and_selections.into_iter(),
                    )?
                    .into(),
                }))
            }
        }

        Ok(Self {
            graph,
            node,
            condition_resolutions_at_node,
            source_entering_condition_resolutions_at_node,
            childs,
        })
    }

    /// Create a path tree by merging the given graph paths.
    /// XXX(@goto-bus-stop): Do we need to keep the FederatedGraphPaths around independently of the
    /// path tree? Or can we just take ownership in this function?
    pub fn from_paths<'a>(
        graph: Arc<FederatedQueryGraph>,
        node: NodeIndex,
        paths: impl Iterator<Item = (&'a FederatedGraphPath, Option<Arc<SelectionSet>>)>,
    ) -> Result<Self, FederationError> {
        Self::from_paths_inner(
            graph,
            node,
            paths.map(|(path, selection_set)| (path.edges(), selection_set)),
        )
    }

    // Path trees should not normally be cloned. This manual implementation is private
    // to prevent external users from cloning.
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            node: self.node,
            source_entering_condition_resolutions_at_node: self
                .source_entering_condition_resolutions_at_node
                .clone(),
            condition_resolutions_at_node: self.condition_resolutions_at_node.clone(),
            childs: self.childs.clone(),
        }
    }

    pub fn merge(&self, other: &Self) -> Self {
        if other.childs.is_empty() {
            return self.clone();
        }
        if self.childs.is_empty() {
            return other.clone();
        }

        let mut merged = self.clone();

        for other_child in &other.childs {
            if let Some(child) = merged
                .childs
                .iter_mut()
                .find(|self_child| self_child.key == other_child.key)
            {
                *child = Arc::new(Child {
                    key: child.key.clone(),
                    // TODO(@goto-bus-stop): pick the lowest cost conditions
                    self_condition_resolutions_for_edge: child
                        .self_condition_resolutions_for_edge
                        .clone(),
                    tree: child.tree.merge(&other_child.tree).into(),
                })
            }
        }

        // TODO(@goto-bus-stop): handle conditions merging

        merged
    }
}
