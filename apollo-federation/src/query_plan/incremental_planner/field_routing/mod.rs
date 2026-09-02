//! Field-level routing as a BULB search space.
//!
//! The planner walks the operation selection-by-selection ("pendings"),
//! consulting the query graph for where each field can be resolved:
//! - [`state`]: mutable search state (pending stack, checkpoints).
//! - [`routing`]: enumerating and ranking options for a selection.
//! - [`commit`]: applying a chosen option to the fetch graph.
//! - [`conditions`]: condition satisfiability for @requires / @key.
//! - [`requires`]: hop-edge inputs and condition paths.
//!
//! This file holds the search-space type and the
//! [`BulbSearchSpace`] implementation.

mod commit;
mod conditions;
mod requires;
mod routing;
pub(super) mod state;

use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::HashSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use routing::RoutingChoice;
pub(crate) use state::PendingSelection;
use state::PlanCheckpoint;
pub(crate) use state::PlanState;
use tracing::debug;
use tracing::trace;

use super::bulb_search::AdvanceResult;
use super::bulb_search::BulbSearchSpace;
use super::shared_path::SharedPath;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::InlineFragment;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionSet;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::QueryPlanCost;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// Cache key for routing options. Captures the selection identity at a QG node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RoutingCacheKey {
    Field(Name),
    InlineFragment(Option<Name>),
}

/// Subgraph, type position, and schema at a query graph node.
pub(super) struct NodeSource {
    pub(super) subgraph: Arc<str>,
    pub(super) type_pos: CompositeTypeDefinitionPosition,
    pub(super) schema: ValidFederationSchema,
}

/// Search space presenting field-level routing decisions as a BULB problem.
pub(crate) struct FieldRoutingSearchSpace {
    pub(crate) query_graph: Arc<QueryGraph>,
    pub(crate) supergraph_schema: ValidFederationSchema,
    pub(crate) override_conditions: OverrideConditions,
    pub(crate) inconsistent_abstract_types: Arc<apollo_compiler::collections::IndexSet<Name>>,
}

impl FieldRoutingSearchSpace {
    pub(super) fn node_source(&self, node: NodeIndex) -> Result<NodeSource, FederationError> {
        let data = self.query_graph.node_weight(node)?;
        Ok(NodeSource {
            subgraph: data.source.clone(),
            type_pos: data.type_.clone().try_into()?,
            schema: self.query_graph.schema_by_source(&data.source)?.clone(),
        })
    }

    /// Select `__typename` in `fetch_node` at `base_path` so the executor
    /// can identify the concrete type for entity representations.
    pub(super) fn append_typename(
        &self,
        state: &mut PlanState,
        fetch_node: NodeIndex,
        base_path: &SharedPath<Arc<OpPathElement>>,
        source: &NodeSource,
    ) {
        let typename = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
            &source.schema,
            &source.type_pos,
            None,
        )));
        state
            .graph
            .append_selection(fetch_node, &base_path.pushed(typename), None);
    }

    /// Op path at which selections enter an entity fetch group: entity
    /// fetches start from the `_Entity` union, so everything nests under a
    /// `... on <ConcreteType>` condition rebased onto the supergraph schema
    /// (which OpPaths reference).
    pub(super) fn entity_root_path(
        &self,
        type_name: &Name,
    ) -> Result<SharedPath<Arc<OpPathElement>>, FederationError> {
        let rebased: CompositeTypeDefinitionPosition =
            self.supergraph_schema.get_type(type_name)?.try_into()?;
        let condition = InlineFragment {
            schema: self.supergraph_schema.clone(),
            parent_type_position: rebased.clone(),
            type_condition_position: Some(rebased),
            directives: Default::default(),
            selection_id: SelectionId::new(),
        };
        Ok(SharedPath::new().pushed(Arc::new(OpPathElement::InlineFragment(condition))))
    }

    /// Can condition fields simply be selected in the fetch at `node`?
    /// True when the subgraph resolves every field itself and none carries
    /// @requires (which draws on an entity representation and needs its own
    /// fetch). The graph-based check complements the schema-based one:
    /// @external fields may still resolve at `node` when it is a
    /// provides-copy created by an ancestor's @provides.
    pub(super) fn can_resolve_in_place(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Result<bool, FederationError> {
        let satisfiable = self.can_satisfy(
            conditions,
            &source.type_pos,
            &source.subgraph,
            &source.schema,
        ) || self.conditions_resolvable_at_node(node, conditions)?;
        Ok(satisfiable && !self.conditions_have_requires(node, conditions)?)
    }

    /// Outgoing edge indices from a query graph node, sorted and filtered.
    pub(super) fn out_edge_indices(&self, node: NodeIndex) -> Vec<EdgeIndex> {
        self.query_graph
            .out_edges(node)
            .into_iter()
            .map(|edge_ref| edge_ref.id())
            .collect()
    }

    /// Find the outgoing edge for a field at a query graph node.
    pub(super) fn edge_for_field(&self, node: NodeIndex, field: &Field) -> Option<EdgeIndex> {
        self.query_graph
            .edge_for_field(node, field, &self.override_conditions)
    }

    /// Find the outgoing downcast edge for an inline fragment at a query
    /// graph node.
    pub(super) fn edge_for_inline_fragment(
        &self,
        node: NodeIndex,
        fragment: &InlineFragment,
    ) -> Option<EdgeIndex> {
        self.query_graph.edge_for_inline_fragment(node, fragment)
    }

    /// Advance through everything that is not a genuine decision: commit
    /// single-option selections and condition pendings greedily, handle
    /// zero-option selections via drops, and lift forced entries above open
    /// decisions so their fetch groups inform scoring. Stops at the first
    /// multi-option ordinary selection.
    ///
    /// Forced commits keep a trail of frames so a drop deeper in the chain
    /// can rewind to an ancestor with untried options (see
    /// `backtrack_forced`): a circular-key hop failing its commit is
    /// often avoidable only by routing an ancestor condition differently,
    /// and no BULB decision frame exists between forced commits to recover
    /// through. The trail is scoped to this call.
    fn fast_forward(&self, state: &mut PlanState) -> Result<(), FederationError> {
        let mut trail = ForcedTrail::default();
        while let Some(top) = state.pending.last() {
            // A site already proven hopeless in this call fails fast:
            // ancestor alternatives can re-push the same doomed selection,
            // and re-proving the dead end from scratch each time would spend
            // the whole backtracking budget without ever ascending to the
            // frame whose alternative actually routes around it.
            if !trail.doomed.is_empty() && trail.doomed.contains(&pending_site(top)) {
                self.recover_doomed(state, &mut trail);
                continue;
            }
            let options = Arc::new(self.routing_options(top)?);
            match options.len() {
                0 => {
                    trail.doomed.insert(pending_site(top));
                    self.recover_doomed(state, &mut trail);
                }
                // A single option is no decision. Condition selections
                // (@requires / @key data) never become decision points even
                // with several options: which subgraph serves them has no
                // plan-shape tradeoff worth beam-searching, and exploring
                // them per probe explodes the search.
                1 => self.commit_forced(state, options, &mut trail),
                _ if top.condition.is_some() => self.commit_forced(state, options, &mut trail),
                _ => {
                    // A BULB decision point. Before stopping, commit any
                    // forced pendings deeper in the stack (single-option,
                    // no-option, or condition selections) so their fetch
                    // groups inform this decision's scoring.
                    let mut lifted = false;
                    for index in (0..state.pending.len().saturating_sub(1)).rev() {
                        let entry = &state.pending[index];
                        if entry.condition.is_some()
                            || Arc::new(self.routing_options(entry)?).len() <= 1
                        {
                            state.lift_pending(index);
                            lifted = true;
                            break;
                        }
                    }
                    if lifted {
                        continue;
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    /// Pop a pending whose site is proven hopeless and recover: rewind an
    /// ancestor forced commit if one has untried options (see
    /// `backtrack_forced`), otherwise drop the selection.
    fn recover_doomed(&self, state: &mut PlanState, trail: &mut ForcedTrail) {
        let pending = state.pop_pending().unwrap();
        if !self.backtrack_forced(state, trail) {
            self.drop_unresolvable(state, &pending);
        }
    }

    /// Pop the top pending selection and commit its best-ranked option.
    ///
    /// A failed commit rolls the whole state back to just after the pop:
    /// `commit_choice` pushes pendings mid-flight, and a graph-only rollback
    /// would leak entries whose `ordering_dependent` names a node index the
    /// rollback freed. A failure first tries the pending's own lower-ranked
    /// options and then ancestor frames via `backtrack_forced`.
    fn commit_forced(
        &self,
        state: &mut PlanState,
        options: Arc<Vec<RoutingChoice>>,
        trail: &mut ForcedTrail,
    ) {
        let pending = state.pop_pending().unwrap();
        let checkpoint = state.checkpoint();
        let result = self.commit_choice(state, &pending, &options[0]);
        if let Err(e) = &result {
            state.rollback(checkpoint.clone());
            debug!(
                selection = %selection_label(&pending.selection),
                subgraph = %options[0].target_subgraph(),
                hop_kind = ?options[0].hop_kind,
                error = ?e,
                "forced commit failed, backtracking",
            );
        }
        let failed = result.is_err();
        if options.len() > 1 {
            trail.frames.push(ForcedFrame {
                pending: pending.clone(),
                options,
                next_option: 1,
                checkpoint,
            });
        } else if failed {
            trail.doomed.insert(pending_site(&pending));
        }
        if failed && !self.backtrack_forced(state, trail) {
            state.dropped_fields += 1;
        }
    }

    /// Rewind the forced-commit trail after a drop and try alternatives,
    /// deepest frame first, each option in rank order. Returns `true` when
    /// the state was rewound, `false` when nothing was attempted.
    fn backtrack_forced(&self, state: &mut PlanState, trail: &mut ForcedTrail) -> bool {
        let mut parked: Option<(Arc<PendingSelection>, RoutingChoice)> = None;
        loop {
            while trail
                .frames
                .last()
                .is_some_and(|f| f.next_option >= f.options.len())
            {
                let exhausted = trail.frames.pop().unwrap();
                trail.doomed.insert(pending_site(&exhausted.pending));
            }
            let Some(frame) = trail.frames.last_mut() else {
                break;
            };
            if state.forced_backtracks >= FORCED_BACKTRACK_CAP {
                break;
            }
            state.forced_backtracks += 1;
            let checkpoint = frame.checkpoint.clone();
            let pending = frame.pending.clone();
            let choice = frame.options[frame.next_option].clone();
            let option_index = frame.next_option;
            frame.next_option += 1;
            state.rollback(checkpoint.clone());
            parked = Some((pending.clone(), frame.options[0].clone()));
            trace!(
                selection = %selection_label(&pending.selection),
                subgraph = %choice.target_subgraph(),
                option_index,
                "backtracking forced commit to alternative option",
            );
            match self.commit_choice(state, &pending, &choice) {
                Ok(_) => return true,
                Err(_) => state.rollback(checkpoint),
            }
        }
        // Give-up after rewinding: re-drive the greedy choice so the
        // caller's loop can continue from a committed state.
        if let Some((pending, choice)) = parked {
            if self.commit_choice(state, &pending, &choice).is_err() {
                state.dropped_fields += 1;
            }
            return true;
        }
        false
    }
}

/// One multi-option forced commit on the fast-forward path, kept so a later
/// drop can rewind to it and try the next-ranked option. Frames are always
/// condition pendings, the class whose greedy routing can strand a
/// descendant on a circular key.
struct ForcedFrame {
    pending: Arc<PendingSelection>,
    options: Arc<Vec<RoutingChoice>>,
    /// Next untried option index; `options[0]` was the greedy choice.
    next_option: usize,
    /// State just after popping `pending`, before any commit.
    checkpoint: PlanCheckpoint,
}

/// Backtracking state for a single `fast_forward` call.
#[derive(Default)]
struct ForcedTrail {
    frames: Vec<ForcedFrame>,
    /// Sites whose every routing option failed during this call. Consulted
    /// before committing so recurring instances fail fast.
    doomed: HashSet<(NodeIndex, RoutingCacheKey)>,
}

/// Identity of a pending's routing position: the query graph node plus the
/// selection's field or type-condition name.
fn pending_site(pending: &PendingSelection) -> (NodeIndex, RoutingCacheKey) {
    let key = match &pending.selection {
        Selection::Field(f) => RoutingCacheKey::Field(f.field.name().clone()),
        Selection::InlineFragment(f) => RoutingCacheKey::InlineFragment(
            f.inline_fragment
                .type_condition_position
                .as_ref()
                .map(|pos| pos.type_name().clone()),
        ),
    };
    (pending.query_graph_node, key)
}

/// Upper bound on forced-commit backtracking attempts per candidate.
const FORCED_BACKTRACK_CAP: u64 = 256;

/// Short human-readable label for a selection, for logging.
pub(super) fn selection_label(selection: &Selection) -> String {
    match selection {
        Selection::Field(f) => f.field.field_position.to_string(),
        Selection::InlineFragment(f) => {
            format!("... on {:?}", f.inline_fragment.type_condition_position)
        }
    }
}

impl BulbSearchSpace for FieldRoutingSearchSpace {
    type Candidate = PlanState;
    /// `Arc` so advance() hands out the stack top in O(1).
    type Decision = Arc<PendingSelection>;
    type Choice = RoutingChoice;
    type Checkpoint = PlanCheckpoint;

    /// Advance past all single-option fields (fast-forward) in place.
    /// Returns the first multi-option decision point, or Complete.
    fn advance(&self, candidate: &mut PlanState) -> AdvanceResult<Arc<PendingSelection>> {
        if let Err(e) = self.fast_forward(candidate) {
            debug!(error = %e, "fast_forward error, completing candidate early");
            // Count every unplanned selection as dropped so cost() keeps
            // this failed candidate below any genuinely complete plan.
            candidate.dropped_fields += candidate.pending.len().max(1);
            while candidate.pop_pending().is_some() {}
            return AdvanceResult::Complete;
        }
        match candidate.pending.last().cloned() {
            Some(decision) => AdvanceResult::Decision(decision),
            None => AdvanceResult::Complete,
        }
    }

    /// Enumerate routing options for a decision.
    fn options(&self, decision: &Arc<PendingSelection>) -> Vec<RoutingChoice> {
        self.routing_options(decision).unwrap_or_default()
    }

    /// Apply a routing choice to the candidate in place. Pops the decision,
    /// commits the choice, and fast-forwards past any resulting single-option
    /// children.
    fn apply(
        &self,
        candidate: &mut PlanState,
        _decision: &Arc<PendingSelection>,
        choice: &RoutingChoice,
    ) {
        // A choice is a unit of effort even when the commit pushes no
        // children (leaf fields); otherwise flat operations register no
        // effort and the search's fuel budget never binds.
        candidate.effort += 1;
        let Some(pending) = candidate.pop_pending() else {
            return;
        };

        trace!(
            selection = %selection_label(&pending.selection),
            target_subgraph = %choice.target_subgraph(),
            hop_kind = ?choice.hop_kind,
            "applying routing choice",
        );

        // Full-state checkpoint: a failed commit_choice may have pushed
        // pendings that must not leak.
        let cp = candidate.checkpoint();
        if let Err(e) = self.commit_choice(candidate, &pending, choice) {
            candidate.rollback(cp);
            debug!(
                selection = %selection_label(&pending.selection),
                subgraph = %choice.target_subgraph(),
                error = ?e,
                "commit_choice failed, dropping field",
            );
            candidate.dropped_fields += 1;
        }

        trace!("partial plan after apply");
    }

    fn checkpoint(&self, candidate: &PlanState) -> PlanCheckpoint {
        candidate.checkpoint()
    }

    fn rollback(&self, candidate: &mut PlanState, cp: PlanCheckpoint) {
        candidate.rollback(cp);
    }

    /// Full deep clone, used only for saving the best complete candidate.
    fn snapshot(&self, candidate: &PlanState) -> PlanState {
        candidate.clone()
    }

    fn is_complete(&self, candidate: &PlanState) -> bool {
        candidate.dropped_fields == 0 && candidate.pending.is_empty()
    }

    fn effort(&self, candidate: &PlanState) -> u64 {
        candidate.effort
    }

    /// Heuristic cost, lower is better. Drop penalties are large but finite
    /// (f64::MAX would prune the state entirely and leave the greedy pass
    /// with no completion when all successors have drops).
    fn cost(&self, candidate: &PlanState) -> QueryPlanCost {
        let base = candidate.graph.cost();
        let cost = base + candidate.dropped_fields as f64 * 1e18;
        trace!(cost, "candidate cost");
        cost
    }
}
