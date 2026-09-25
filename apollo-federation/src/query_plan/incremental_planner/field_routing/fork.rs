//! Forking a field across subgraphs. A keyless value type shared by several
//! subgraphs has no key hop, so a child the chosen subgraph lacks can only
//! be reached by fetching the parent field again elsewhere. The fork is
//! decided where the parent field is routed: the option whose target
//! strands part of the subtree is replaced by a [`RoutingChoice::Fork`]
//! that commits the reachable part and re-pushes the rest, pinned to a
//! subgraph that can serve it.

use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::ArcKey;
use super::FieldRoutingSearchSpace;
use super::RoutingSiteKey;
use super::routing::RoutingChoice;
use super::selection_label;
use super::state::PendingSelection;
use super::state::PlanState;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::HasSelectionKey;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;

/// The part of a forked field one alternative subgraph serves.
#[derive(Clone, Debug)]
pub(crate) struct ForkRemainder {
    pub(crate) target_subgraph: Arc<str>,
    /// The parent field wrapping only the children this subgraph serves.
    pub(crate) selection: Selection,
}

impl FieldRoutingSearchSpace {
    /// Replace every edge option whose target strands part of `pending`'s
    /// subtree with a fork, when another option's target can serve the
    /// stranded part. Options that strand nothing recoverable stay as they
    /// are, so the option count never changes and forcedness is unaffected.
    pub(super) fn fork_stranded_children(
        &self,
        pending: &PendingSelection,
        options: &mut [RoutingChoice],
    ) -> Result<(), FederationError> {
        let Selection::Field(field) = &pending.selection else {
            return Ok(());
        };
        let Some(sub_ss) = field.selection_set.as_ref().filter(|ss| !ss.is_empty()) else {
            return Ok(());
        };
        if !self.root_may_duplicate(pending.query_graph_node)? {
            return Ok(());
        }
        let targets = self.option_targets(options)?;
        let distinct = targets.iter().flatten().collect::<hashbrown::HashSet<_>>();
        if distinct.len() < 2 {
            return Ok(());
        }
        for index in 0..options.len() {
            let Some(target) = targets[index] else {
                continue;
            };
            let stranded = self.stranded_at(target, sub_ss)?;
            if stranded.is_empty() {
                continue;
            }
            let remainders =
                self.assign_remainders(&pending.selection, &stranded, target, options, &targets)?;
            if remainders.is_empty() {
                continue;
            }
            // A primary left with nothing to fetch is not a fork, it is the
            // wrong subgraph; the alternatives are already in the pool.
            let kept = remainders
                .iter()
                .try_fold(pending.selection.clone(), |kept, r| {
                    remaining_after_split(&kept, std::slice::from_ref(&r.selection))
                });
            let Some(kept) = kept else {
                continue;
            };
            options[index] = RoutingChoice::Fork {
                primary: Box::new(options[index].clone()),
                kept,
                remainders,
            };
        }
        Ok(())
    }

    /// Query graph target node per option, `None` for options without a
    /// committable edge.
    fn option_targets(
        &self,
        options: &[RoutingChoice],
    ) -> Result<Vec<Option<NodeIndex>>, FederationError> {
        options
            .iter()
            .map(|opt| {
                if opt.conditions_unroutable() {
                    return Ok(None);
                }
                let Some(edge) = opt.edge_index() else {
                    return Ok(None);
                };
                Ok(Some(self.qg().edge_endpoints(edge)?.1))
            })
            .collect()
    }

    /// Group the children stranded at `primary_target` by the best-ranked
    /// alternative whose target serves each one whole. Children no
    /// alternative serves stay with the primary.
    fn assign_remainders(
        &self,
        parent: &Selection,
        stranded: &[Selection],
        primary_target: NodeIndex,
        options: &[RoutingChoice],
        targets: &[Option<NodeIndex>],
    ) -> Result<Vec<ForkRemainder>, FederationError> {
        let mut groups: Vec<(Arc<str>, NodeIndex, Vec<Selection>)> = Vec::new();
        for child in stranded {
            let mut serving = None;
            for (opt, target) in options.iter().zip(targets) {
                let Some(target) = *target else {
                    continue;
                };
                if target == primary_target {
                    continue;
                }
                if self.stranded_selection(target, child)?.is_none() {
                    serving = Some((opt.target_subgraph().clone(), target));
                    break;
                }
            }
            let Some((subgraph, target)) = serving else {
                continue;
            };
            match groups.iter_mut().find(|(_, node, _)| *node == target) {
                Some((_, _, children)) => children.push(child.clone()),
                None => groups.push((subgraph, target, vec![child.clone()])),
            }
        }
        groups
            .into_iter()
            .map(|(target_subgraph, _, children)| {
                Ok(ForkRemainder {
                    target_subgraph,
                    selection: rewrap(parent, children)?,
                })
            })
            .collect()
    }

    /// Children of `sub_ss` no fetch positioned at `node` can reach at any
    /// depth, in the parent's shape: a partially stranded field comes back
    /// wrapping only its stranded part. The walk descends through keyless
    /// positions only. A child with a key hop of its own is left alone,
    /// since routing that child is a decision the planner already makes.
    pub(super) fn stranded_at(
        &self,
        node: NodeIndex,
        sub_ss: &SelectionSet,
    ) -> Result<Arc<Vec<Selection>>, FederationError> {
        let key = (node, ArcKey::new(&sub_ss.selections));
        if let Some(cached) = self.caches.stranded.borrow().get(&key) {
            return Ok(cached.clone());
        }
        let mut stranded = Vec::new();
        for sel in sub_ss.selections.values() {
            if let Some(part) = self.stranded_selection(node, sel)? {
                stranded.push(part);
            }
        }
        let stranded = Arc::new(stranded);
        self.caches
            .stranded
            .borrow_mut()
            .insert(key, stranded.clone());
        Ok(stranded)
    }

    /// The stranded part of one selection at `node`, `None` when all of it
    /// is reachable.
    fn stranded_selection(
        &self,
        node: NodeIndex,
        sel: &Selection,
    ) -> Result<Option<Selection>, FederationError> {
        match sel {
            Selection::Field(child) => {
                if *child.field.name() == TYPENAME_FIELD {
                    return Ok(None);
                }
                let has_hop = !self.field_key_hops(node, &child.field)?.is_empty();
                match self.cached_query_graph.edge_for_field(node, &child.field) {
                    None if has_hop => Ok(None),
                    None => Ok(Some(sel.clone())),
                    Some(_) if has_hop => Ok(None),
                    Some(edge) => {
                        let Some(child_ss) = child.selection_set.as_ref() else {
                            return Ok(None);
                        };
                        let (_, child_node) = self.qg().edge_endpoints(edge)?;
                        let inner = self.stranded_at(child_node, child_ss)?;
                        if inner.is_empty() {
                            return Ok(None);
                        }
                        Ok(Some(rewrap(sel, inner.iter().cloned())?))
                    }
                }
            }
            Selection::InlineFragment(frag) => {
                let frag_node = match self
                    .cached_query_graph
                    .edge_for_inline_fragment(node, &frag.inline_fragment)
                {
                    Some(edge) => self.qg().edge_endpoints(edge)?.1,
                    None => node,
                };
                let inner = self.stranded_at(frag_node, &frag.selection_set)?;
                if inner.is_empty() {
                    return Ok(None);
                }
                Ok(Some(rewrap(sel, inner.iter().cloned())?))
            }
        }
    }

    /// Key hops from `node` that reach a position defining `field`.
    fn field_key_hops(
        &self,
        node: NodeIndex,
        field: &Field,
    ) -> Result<Arc<Vec<RoutingChoice>>, FederationError> {
        let key = RoutingSiteKey::Field(field.name().clone());
        self.cached_key_hops(node, None, key, |key_target| {
            self.cached_query_graph.edge_for_field(key_target, field)
        })
    }

    /// Commit a fork: the primary choice fetches `kept`, and each remainder
    /// is re-pushed at the same position pinned to its serving subgraph.
    pub(super) fn commit_fork(
        &self,
        state: &mut PlanState,
        pending: &PendingSelection,
        primary: &RoutingChoice,
        kept: &Selection,
        remainders: &[ForkRemainder],
    ) -> Result<(), FederationError> {
        for remainder in remainders.iter().rev() {
            tracing::trace!(
                field = %selection_label(&pending.selection),
                remainder = %selection_label(&remainder.selection),
                subgraph = %remainder.target_subgraph,
                "forking field to another subgraph for stranded children",
            );
            state.push_pending(
                pending
                    .fork(remainder.selection.clone())
                    .with_restrict_to(Some(remainder.target_subgraph.clone())),
            );
        }
        self.commit_choice(state, &pending.fork(kept.clone()), primary)
    }
}

/// `sel` with its sub-selection replaced by `children`.
fn rewrap(
    sel: &Selection,
    children: impl IntoIterator<Item = Selection>,
) -> Result<Selection, FederationError> {
    let type_position = sel
        .selection_set()
        .ok_or_else(|| FederationError::internal("rewrapping a leaf selection"))?
        .type_position
        .clone();
    sel.with_updated_selections(type_position, children)
}

/// What remains of `sel` after removing the split-off subset sharing its
/// selection key, recursing into sub-selections. `None` means the
/// selection moved entirely.
pub(super) fn remaining_after_split(sel: &Selection, split: &[Selection]) -> Option<Selection> {
    let Some(moved) = split.iter().find(|s| s.key() == sel.key()) else {
        return Some(sel.clone());
    };
    let (Some(orig_ss), Some(moved_ss)) = (sel.selection_set(), moved.selection_set()) else {
        return None;
    };
    let moved_children: Vec<Selection> = moved_ss.selections.values().cloned().collect();
    let kept: Vec<Selection> = orig_ss
        .selections
        .values()
        .filter_map(|child| remaining_after_split(child, &moved_children))
        .collect();
    if kept.is_empty() {
        return None;
    }
    rewrap(sel, kept).ok()
}
