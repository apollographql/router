use crate::error::FederationError;
use crate::query_graph::graph_path::GraphPathItem;
use crate::query_graph::graph_path::OpGraphPath;
use crate::query_graph::graph_path::OpGraphPathTrigger;
use crate::query_graph::QueryGraph;
use crate::query_plan::operation::NormalizedSelectionSet;
use indexmap::map::Entry;
use indexmap::IndexMap;
use petgraph::graph::{EdgeIndex, NodeIndex};
use std::hash::Hash;
use std::sync::Arc;

/// A "merged" tree representation for a vector of `GraphPath`s that start at a common query graph
/// node, in which each node of the tree corresponds to a node in the query graph, and a tree's node
/// has a child for every unique pair of edge and trigger.
// PORT_NOTE: The JS codebase additionally has a property `triggerEquality`; this existed because
// Typescript doesn't have a native way of associating equality/hash functions with types, so they
// were passed around manually. This isn't the case with Rust, where we instead implement trigger
// equality via `PartialEq` and `Hash`.
#[derive(Debug)]
pub(crate) struct PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    /// The query graph of which this is a path tree.
    graph: Arc<QueryGraph>,
    /// The query graph node at which the path tree starts.
    node: NodeIndex,
    /// Note that `ClosedPath`s have an optimization which splits them into paths and a selection
    /// set representing a trailing query to a single subgraph at the final nodes of the paths. For
    /// such paths where this `PathTree`'s node corresponds to that final node, those selection sets
    /// are collected here. This is really an optimization to avoid unnecessary merging of selection
    /// sets when they query a single subgraph.
    local_selection_sets: Vec<Arc<NormalizedSelectionSet>>,
    /// The child `PathTree`s for this `PathTree` node. There is a child for every unique pair of
    /// edge and trigger present at this particular sub-path within the `GraphPath`s covered by this
    /// `PathTree` node.
    childs: Vec<Arc<PathTreeChild<TTrigger, TEdge>>>,
}

#[derive(Debug)]
pub(crate) struct PathTreeChild<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    /// The edge connecting this child to its parent.
    edge: TEdge,
    /// The trigger for the edge connecting this child to its parent.
    trigger: Arc<TTrigger>,
    /// The conditions required to be fetched if this edge is taken.
    conditions: Option<Arc<OpPathTree>>,
    /// The child `PathTree` reached by taking the edge.
    tree: Arc<PathTree<TTrigger, TEdge>>,
}

/// A `PathTree` whose triggers are operation elements (essentially meaning that the constituent
/// `GraphPath`s were guided by a GraphQL operation).
pub(crate) type OpPathTree = PathTree<OpGraphPathTrigger, Option<EdgeIndex>>;

impl OpPathTree {
    pub(crate) fn new(graph: Arc<QueryGraph>, node: NodeIndex) -> Self {
        Self {
            graph,
            node,
            local_selection_sets: Vec::new(),
            childs: Vec::new(),
        }
    }

    pub(crate) fn from_op_paths(
        graph: Arc<QueryGraph>,
        node: NodeIndex,
        paths: &[(&OpGraphPath, &Arc<NormalizedSelectionSet>)],
    ) -> Result<Self, FederationError> {
        assert!(
            !paths.is_empty(),
            "OpPathTree cannot be created from an empty set of paths"
        );
        Self::from_paths(
            graph,
            node,
            paths
                .iter()
                .map(|(path, selections)| (path.iter(), *selections))
                .collect::<Vec<_>>(),
        )
    }
}

impl<TTrigger, TEdge> PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Hash + Eq + Into<Option<EdgeIndex>>,
{
    fn from_paths<'inputs>(
        graph: Arc<QueryGraph>,
        node: NodeIndex,
        graph_paths_and_selections: Vec<(
            impl Iterator<Item = GraphPathItem<'inputs, TTrigger, TEdge>>,
            &'inputs Arc<NormalizedSelectionSet>,
        )>,
    ) -> Result<Self, FederationError>
    where
        TTrigger: 'inputs,
        TEdge: 'inputs,
    {
        // Group by and order by unique edge ID, and among those by unique trigger
        let mut merged = IndexMap::<TEdge, ByUniqueEdge<TTrigger, /* impl Iterator */ _>>::new();

        struct ByUniqueEdge<'inputs, TTrigger, GraphPathIter> {
            target_node: NodeIndex,
            by_unique_trigger:
                IndexMap<&'inputs Arc<TTrigger>, PathTreeChildInputs<'inputs, GraphPathIter>>,
        }

        struct PathTreeChildInputs<'inputs, GraphPathIter> {
            conditions: Option<Arc<OpPathTree>>,
            sub_paths_and_selections: Vec<(GraphPathIter, &'inputs Arc<NormalizedSelectionSet>)>,
        }

        let mut local_selection_sets = Vec::new();

        for (mut graph_path_iter, selection) in graph_paths_and_selections {
            let Some((generic_edge, trigger, conditions)) = graph_path_iter.next() else {
                // End of an input `GraphPath`
                local_selection_sets.push(selection.clone());
                continue;
            };
            let for_edge = match merged.entry(generic_edge) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    entry.insert(ByUniqueEdge {
                        target_node: if let Some(edge) = generic_edge.into() {
                            let (_source, target) = graph.edge_endpoints(edge)?;
                            target
                        } else {
                            // For a "None" edge, stay on the same node
                            node
                        },
                        by_unique_trigger: IndexMap::new(),
                    })
                }
            };
            match for_edge.by_unique_trigger.entry(trigger) {
                Entry::Occupied(entry) => {
                    let existing = entry.into_mut();
                    existing.conditions = merge_conditions(&existing.conditions, conditions);
                    existing
                        .sub_paths_and_selections
                        .push((graph_path_iter, selection))
                    // Note that as we merge, we don't create a new child
                }
                Entry::Vacant(entry) => {
                    entry.insert(PathTreeChildInputs {
                        conditions: conditions.clone(),
                        sub_paths_and_selections: vec![(graph_path_iter, selection)],
                    });
                }
            }
        }

        let mut childs = Vec::new();
        for (edge, by_unique_edge) in merged {
            for (trigger, child) in by_unique_edge.by_unique_trigger {
                childs.push(Arc::new(PathTreeChild {
                    edge,
                    trigger: trigger.clone(),
                    conditions: child.conditions.clone(),
                    tree: Arc::new(Self::from_paths(
                        graph.clone(),
                        by_unique_edge.target_node,
                        child.sub_paths_and_selections,
                    )?),
                }))
            }
        }
        Ok(Self {
            graph,
            node,
            local_selection_sets,
            childs,
        })
    }

    fn merge_if_not_equal(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
        if self.equals_same_root(other) {
            self.clone()
        } else {
            self.merge(other)
        }
    }

    /// May have false negatives (see comment about `Arc::ptr_eq`)
    fn equals_same_root(self: &Arc<Self>, other: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, other)
            || self.childs.iter().zip(&other.childs).all(|(a, b)| {
                a.edge == b.edge
                    // `Arc::ptr_eq` instead of `==` is faster and good enough.
                    // This method is all about avoid unnecessary merging
                    // when we suspect conditions trees have been build from the exact same inputs.
                    && Arc::ptr_eq(&a.trigger, &b.trigger)
                    && match (&a.conditions, &b.conditions) {
                        (None, None) => true,
                        (Some(cond_a), Some(cond_b)) => cond_a.equals_same_root(cond_b),
                        _ => false,
                    }
                    && a.tree.equals_same_root(&b.tree)
            })
    }

    fn merge(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
        if Arc::ptr_eq(self, other) {
            return self.clone();
        }
        assert!(
            Arc::ptr_eq(&self.graph, &other.graph),
            "Cannot merge path tree build on another graph"
        );
        assert_eq!(
            self.node, other.node,
            "Cannot merge path trees rooted different nodes"
        );
        if other.childs.is_empty() {
            return self.clone();
        }
        if self.childs.is_empty() {
            return other.clone();
        }

        let mut count_to_add = 0;
        let merge_indices: Vec<_> = other
            .childs
            .iter()
            .map(|other_child| {
                let position = self.childs.iter().position(|self_child| {
                    self_child.edge == other_child.edge && self_child.trigger == other_child.trigger
                });
                if position.is_none() {
                    count_to_add += 1
                }
                position
            })
            .collect();
        let expected_new_len = self.childs.len() + count_to_add;
        let mut childs = Vec::with_capacity(expected_new_len);
        childs.extend(self.childs.iter().cloned());
        for (other_child, merge_index) in other.childs.iter().zip(merge_indices) {
            if let Some(i) = merge_index {
                let child = &mut childs[i];
                *child = Arc::new(PathTreeChild {
                    edge: child.edge,
                    trigger: child.trigger.clone(),
                    conditions: merge_conditions(&child.conditions, &other_child.conditions),
                    tree: child.tree.merge(&other_child.tree),
                })
            } else {
                childs.push(other_child.clone())
            }
        }
        assert_eq!(childs.len(), expected_new_len);

        Arc::new(Self {
            graph: self.graph.clone(),
            node: self.node,
            local_selection_sets: self
                .local_selection_sets
                .iter()
                .chain(&other.local_selection_sets)
                .cloned()
                .collect(),
            childs,
        })
    }
}

fn merge_conditions(
    a: &Option<Arc<OpPathTree>>,
    b: &Option<Arc<OpPathTree>>,
) -> Option<Arc<OpPathTree>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.merge_if_not_equal(b)),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}
