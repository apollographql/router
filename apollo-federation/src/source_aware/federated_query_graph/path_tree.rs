use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;

use crate::error::FederationError;
use crate::operation::SelectionSet;
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
    childs: Vec<Child>,
}

#[derive(Debug, Clone)] // XXX(@goto-bus-stop): do we want to clone this?
pub(crate) struct Child {
    key: ChildKey,
    /// Conditions are imposed by query graph edges and resolved at earlier query graph nodes. When
    /// resolution happens for this edge's condition during option generation, that resolution's ID
    /// is stored in the graph path's edge's `self_condition_resolutions_for_edge`, which gets
    /// merged into this map during path tree generation.
    self_condition_resolutions_for_edge: IndexMap<SelfConditionIndex, ConditionResolutionId>,
    tree: Arc<FederatedPathTree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildKey {
    pub(crate) operation_element: Option<Arc<OperationPathElement>>,
    pub(crate) edge: Option<EdgeIndex>,
}

/// Merge two maps of conditions at a node, keeping the cheapest way to resolve each condition if
/// there is a conflict.
///
/// `remapped_condition_ids` is a map of "losing" condition IDs to "winning" condition IDs in case
/// of conflicts. This can be used to replace references to the "losing" conditions by the "winning"
/// ones down the tree.
fn merge_condition_resolutions(
    condition_resolutions: &mut IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    other_condition_resolutions: &IndexMap<SelfConditionIndex, ConditionResolutionInfo>,
    remapped_condition_ids: &mut HashMap<ConditionResolutionId, ConditionResolutionId>,
) {
    for (&condition_index, resolution) in other_condition_resolutions {
        condition_resolutions
            .entry(condition_index)
            .and_modify(|existing_resolution| {
                if existing_resolution.cost > resolution.cost {
                    remapped_condition_ids.insert(existing_resolution.id, resolution.id);
                    existing_resolution.clone_from(resolution);
                } else {
                    remapped_condition_ids.insert(resolution.id, existing_resolution.id);
                }
            })
            .or_insert_with(|| resolution.clone());
    }
}

fn merge_and_remap_condition_resolution_ids(
    existing: &mut IndexMap<SelfConditionIndex, ConditionResolutionId>,
    other: &IndexMap<SelfConditionIndex, ConditionResolutionId>,
    remapped_condition_ids: &HashMap<ConditionResolutionId, ConditionResolutionId>,
) {
    // Resolution IDs need to be remapped, but it needs to be done for *every* ID in the
    // merged map, so it's easier to first combine the maps and then patch up the IDs afterward.
    existing.extend(
        other
            .iter()
            .map(|(condition_index, resolution_id)| (*condition_index, *resolution_id)),
    );

    for condition_id in existing.values_mut() {
        if let Some(remapped_id) = remapped_condition_ids.get(condition_id) {
            *condition_id = *remapped_id;
        }
    }
}

impl FederatedPathTree {
    // This is not generic in practice: the EdgeIter and PathIter types are just not
    // nameable.
    //
    // Each recursive call to `from_paths_inner` advances all `EdgeIter`s once, in lockstep. Each
    // iteration adds one depth level to its subtree, potentially many nodes side-by-side.
    //
    // XXX(@goto-bus-stop): can this be simplified by walking each path in order? i.e. depth-first
    // rather than breadth-first.
    fn from_paths_inner<'a, EdgeIter, PathIter>(
        graph: Arc<FederatedQueryGraph>,
        node: NodeIndex,
        paths: PathIter,
        remapped_condition_ids: &mut HashMap<ConditionResolutionId, ConditionResolutionId>,
    ) -> Result<Self, FederationError>
    where
        EdgeIter: Iterator<Item = &'a graph_path::Edge>,
        PathIter: Iterator<Item = (EdgeIter, Option<Arc<SelectionSet>>)>,
    {
        let mut condition_resolutions_at_node = IndexMap::new();
        let mut source_entering_condition_resolutions_at_node = IndexMap::new();

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
        for (mut graph_path_iter, selection) in paths {
            let Some(edge) = graph_path_iter.next() else {
                continue;
            };

            merge_condition_resolutions(
                &mut source_entering_condition_resolutions_at_node,
                &edge.source_entering_condition_resolutions_at_head,
                remapped_condition_ids,
            );
            merge_condition_resolutions(
                &mut condition_resolutions_at_node,
                &edge.condition_resolutions_at_head,
                remapped_condition_ids,
            );

            // Marginally inefficient: we look up edge endpoints even if the edge already exists.
            // This is to make the ? error propagate.
            // XXX(@goto-bus-stop): Should we store the endpoint on the graph path edges? Then this
            // code would not need to return a result. The query graph is not mutable at this point so
            // it can not get out of sync.
            let for_edge = merged.entry(edge.edge).or_insert(ByUniqueEdge {
                target_node: if let Some(edge) = &edge.edge {
                    let (_source, target) = graph.edge_endpoints(*edge)?;
                    target
                } else {
                    // For a "None" edge, stay on the same node
                    node
                },
                by_unique_trigger: IndexMap::new(),
            });

            match for_edge
                .by_unique_trigger
                .entry(edge.operation_element.clone())
            {
                indexmap::map::Entry::Occupied(existing) => {
                    let existing = existing.into_mut();
                    merge_and_remap_condition_resolution_ids(
                        &mut existing.self_condition_resolutions_for_edge,
                        &edge.self_condition_resolutions_for_edge,
                        remapped_condition_ids,
                    );
                    existing
                        .sub_paths_and_selections
                        .push((graph_path_iter, selection));
                }
                indexmap::map::Entry::Vacant(vacant) => {
                    vacant.insert(PathTreeChildInputs {
                        self_condition_resolutions_for_edge: edge
                            .self_condition_resolutions_for_edge
                            .clone(),
                        sub_paths_and_selections: vec![(graph_path_iter, selection)],
                    });
                }
            }
        }

        let mut childs = vec![];
        for (edge, by_unique_edge) in merged {
            for (operation_element, child) in by_unique_edge.by_unique_trigger {
                childs.push(Child {
                    key: ChildKey {
                        operation_element,
                        edge,
                    },
                    self_condition_resolutions_for_edge: child.self_condition_resolutions_for_edge,
                    tree: Self::from_paths_inner(
                        graph.clone(),
                        by_unique_edge.target_node,
                        child.sub_paths_and_selections.into_iter(),
                        remapped_condition_ids,
                    )?
                    .into(),
                })
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
    pub fn from_paths<'a>(
        graph: Arc<FederatedQueryGraph>,
        node: NodeIndex,
        paths: impl Iterator<Item = (&'a FederatedGraphPath, Option<Arc<SelectionSet>>)>,
    ) -> Result<Self, FederationError> {
        Self::from_paths_inner(
            graph,
            node,
            paths.map(|(path, selection_set)| (path.edges(), selection_set)),
            &mut Default::default(),
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

    fn merge_inner(
        self: &Arc<Self>,
        other: &Arc<Self>,
        remapped_condition_ids: &mut HashMap<ConditionResolutionId, ConditionResolutionId>,
    ) -> Arc<Self> {
        if other.childs.is_empty() {
            return self.clone();
        }
        if self.childs.is_empty() {
            return other.clone();
        }

        let mut merged = (**self).clone();

        merge_condition_resolutions(
            &mut merged.source_entering_condition_resolutions_at_node,
            &other.source_entering_condition_resolutions_at_node,
            remapped_condition_ids,
        );
        merge_condition_resolutions(
            &mut merged.condition_resolutions_at_node,
            &other.condition_resolutions_at_node,
            remapped_condition_ids,
        );

        for other_child in &other.childs {
            if let Some(child) = merged
                .childs
                .iter_mut()
                .find(|self_child| self_child.key == other_child.key)
            {
                merge_and_remap_condition_resolution_ids(
                    &mut child.self_condition_resolutions_for_edge,
                    &other_child.self_condition_resolutions_for_edge,
                    remapped_condition_ids,
                );
                child.tree = child
                    .tree
                    .merge_inner(&other_child.tree, remapped_condition_ids);
            }
        }

        merged.into()
    }

    pub fn merge(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
        self.merge_inner(other, &mut Default::default())
    }
}
