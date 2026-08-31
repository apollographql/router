//! Field-level routing as a BULB search space.
//!
//! The planner walks the operation selection-by-selection ("pendings"),
//! consulting the query graph for where each field can be resolved:
//! - [`state`]: mutable search state (pending stack, checkpoints).
//! - [`conditions`]: condition satisfiability for @requires / @key.
//! - [`requires`]: hop-edge inputs and condition paths.
//!
//! This file holds the search-space type. Routing enumeration, commit
//! logic, and the BulbSearchSpace implementation build on this skeleton
//! in later changes.

pub(super) mod cached_query_graph;
mod commit;
mod conditions;
pub(super) mod context;
mod requires;
mod routing;
pub(super) mod state;
#[cfg(test)]
mod test_support;
mod type_conditions;

use std::cell::RefCell;
use std::sync::Arc;

use apollo_compiler::Name;
use cached_query_graph::CachedQueryGraph;
use hashbrown::HashMap;
use hashbrown::HashSet;
use petgraph::graph::NodeIndex;
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
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::QueryPlanCost;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

// ---------------------------------------------------------------------------
// Cache key types
// ---------------------------------------------------------------------------

/// Cache key comparing/hashing by `Arc` pointer identity while owning the
/// `Arc`: ownership keeps the allocation alive for the cache's lifetime, so
/// the address can't be reused after a drop.
pub(super) struct ArcKey<T>(Arc<T>);

impl<T> ArcKey<T> {
    pub(super) fn new(value: &Arc<T>) -> Self {
        Self(value.clone())
    }
}

impl<T> Clone for ArcKey<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> PartialEq for ArcKey<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for ArcKey<T> {}

impl<T> std::hash::Hash for ArcKey<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

pub(super) type ConditionsKey = ArcKey<SelectionSet>;

/// Pointer-identity key for a `Selection`, owning the inner Arc.
#[allow(dead_code)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum SelectionArcKey {
    Field(ArcKey<crate::operation::FieldSelection>),
    InlineFragment(ArcKey<crate::operation::InlineFragmentSelection>),
}

#[allow(dead_code)]
impl SelectionArcKey {
    pub(super) fn new(selection: &Selection) -> Self {
        match selection {
            Selection::Field(field) => Self::Field(ArcKey::new(field)),
            Selection::InlineFragment(frag) => Self::InlineFragment(ArcKey::new(frag)),
        }
    }
}

// ---------------------------------------------------------------------------
// Planner caches
// ---------------------------------------------------------------------------

type RoutingOptionsCache = RefCell<
    HashMap<
        (
            NodeIndex,
            SelectionArcKey,
            Option<ArcKey<std::collections::HashSet<Name>>>,
        ),
        Arc<Vec<RoutingChoice>>,
    >,
>;

type KeyHopCache = RefCell<HashMap<(NodeIndex, RoutingSiteKey), Arc<Vec<RoutingChoice>>>>;
type CanSatisfyCache = RefCell<HashMap<(ConditionsKey, Name, Arc<str>), bool>>;
type ConditionsRoutableCache =
    RefCell<HashMap<(NodeIndex, ArcKey<crate::operation::SelectionMap>), bool>>;

/// Monotonically-growing caches for computations that depend on search-space
/// state or that reference routing types. These live on
/// FieldRoutingSearchSpace (not PlanState) so checkpoint/rollback never
/// touches them.
#[allow(dead_code)]
pub(super) struct PlannerCaches {
    pub(super) routing_options: RoutingOptionsCache,
    key_hops: KeyHopCache,
    pub(super) can_satisfy: CanSatisfyCache,
    pub(super) conditions_routable: ConditionsRoutableCache,
    pub(super) key_hops_in_flight: RefCell<HashSet<(NodeIndex, RoutingSiteKey)>>,
    pub(super) guard_hits: std::cell::Cell<u64>,
}

impl PlannerCaches {
    pub(crate) fn new() -> Self {
        Self {
            routing_options: RefCell::new(HashMap::new()),
            key_hops: RefCell::new(HashMap::new()),
            can_satisfy: RefCell::new(HashMap::new()),
            conditions_routable: RefCell::new(HashMap::new()),
            key_hops_in_flight: RefCell::new(HashSet::new()),
            guard_hits: std::cell::Cell::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Search space
// ---------------------------------------------------------------------------

/// Type position and schema at a query graph node.
pub(super) struct NodeSource {
    pub(super) subgraph: Arc<str>,
    pub(super) type_pos: CompositeTypeDefinitionPosition,
    pub(super) schema: ValidFederationSchema,
}

/// Search space presenting field-level routing decisions as a BULB problem.
pub(crate) struct FieldRoutingSearchSpace {
    pub(crate) cached_query_graph: CachedQueryGraph,
    pub(crate) supergraph_schema: ValidFederationSchema,
    pub(super) caches: PlannerCaches,
    /// Subgraphs the caller disabled: enumeration never routes into them.
    pub(crate) disabled_subgraphs: apollo_compiler::collections::IndexSet<Arc<str>>,
}

/// Identity of the selection a key-hop enumeration serves; paired with the
/// origin node in the in-flight cycle guard.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RoutingSiteKey {
    Field(Name),
    InlineFragment(Option<Name>),
}

impl FieldRoutingSearchSpace {
    pub(super) fn qg(&self) -> &crate::query_graph::QueryGraph {
        &self.cached_query_graph.query_graph
    }

    pub(super) fn node_source(&self, node: NodeIndex) -> Result<NodeSource, FederationError> {
        let data = self.qg().node_weight(node)?;
        Ok(NodeSource {
            subgraph: data.source.clone(),
            type_pos: data.type_.clone().try_into()?,
            schema: self.qg().schema_by_source(&data.source)?.clone(),
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
    /// the schema check rejects @external fields, but they may still
    /// resolve at `node` when it is a provides-copy created by an
    /// ancestor's @provides, which only the query graph knows about.
    pub(super) fn can_resolve_in_place(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Result<bool, FederationError> {
        let satisfiable = self.cached_can_satisfy(
            conditions,
            &source.type_pos,
            &source.subgraph,
            &source.schema,
        ) || self.conditions_resolvable_at_node(node, conditions)?;
        Ok(satisfiable && !self.conditions_have_requires(node, conditions)?)
    }

    /// Walk up the split_parent chain to find an ancestor field with
    /// routing options to a different subgraph, wrapping the stranded
    /// remainder back into the ancestor's selection shape.
    fn try_split_repush(&self, state: &mut PlanState, pending: &PendingSelection) -> bool {
        if !state.split_repush_enabled || pending.best_effort {
            return false;
        }
        let Ok(source) = self.node_source(pending.query_graph_node) else {
            return false;
        };
        let avoid = source.subgraph;
        let mut remainder: Vec<Selection> = vec![pending.selection.clone()];
        let mut link = pending.split_parent.clone();
        while let Some(anchor) = link {
            let Some(template) = anchor.selection.selection_set() else {
                return false;
            };
            let wrapped = if template.type_position.is_abstract_type() {
                anchor.selection.clone()
            } else {
                let Some(wrapped) = wrap_in_parent(&anchor.selection, &remainder) else {
                    return false;
                };
                wrapped
            };
            if matches!(anchor.selection, Selection::Field(_)) && anchor.condition.is_none() {
                let mut candidate = anchor
                    .fork(wrapped.clone())
                    .with_split_avoid(Some(avoid.clone()));
                candidate.condition = pending.condition;
                let has_alternative = self
                    .cached_routing_options(&candidate)
                    .is_ok_and(|options| !options.is_empty());
                if has_alternative {
                    let Some(anchor_ss) = anchor.selection.selection_set() else {
                        return false;
                    };
                    let Some(remainder_ss) = wrapped.selection_set() else {
                        return false;
                    };
                    if routing::selection_leaf_count(remainder_ss)
                        >= routing::selection_leaf_count(anchor_ss)
                    {
                        return false;
                    }
                    debug!(
                        selection = %selection_label(&pending.selection),
                        anchor = %selection_label(&anchor.selection),
                        avoid = %avoid,
                        "re-pushing stranded remainder at ancestor with alternatives",
                    );
                    state.splits += 1;
                    state.push_pending(candidate);
                    return true;
                }
            }
            remainder = vec![wrapped];
            link = anchor.split_parent.clone();
        }
        false
    }

    /// Advance past deterministic decisions in-place: commit single-option
    /// and zero-option selections, lift forced entries above open decisions
    /// so their fetch groups inform scoring. Stops at the first multi-option
    /// ordinary selection.
    ///
    /// Forced commits keep a trail of frames so a drop deeper in the chain
    /// can rewind to an ancestor with untried options: a circular-key hop
    /// failing its commit is often avoidable only by routing an ancestor
    /// condition differently, and no BULB decision frame exists between
    /// forced commits to recover through.
    fn fast_forward(&self, state: &mut PlanState) -> Result<(), FederationError> {
        let mut trail = ForcedTrail::default();
        while let Some(top) = state.pending.last() {
            if !trail.doomed.is_empty() && trail.doomed.contains(&pending_site(top)) {
                self.recover_doomed(state, &mut trail);
                continue;
            }
            let options: Arc<Vec<RoutingChoice>> = Arc::new(self.routing_options(top)?);
            match options.len() {
                0 => {
                    trail.doomed.insert(pending_site(top));
                    self.recover_doomed(state, &mut trail);
                }
                1 => self.commit_forced(state, options, &mut trail),
                _ if top.condition.is_some() => self.commit_forced(state, options, &mut trail),
                _ => {
                    // A BULB decision point. Before stopping, commit any
                    // forced pendings deeper in the stack so their fetch
                    // groups inform this decision's scoring.
                    let mut lifted = false;
                    for index in (0..state.pending.len().saturating_sub(1)).rev() {
                        let entry = &state.pending[index];
                        if entry.condition.is_some() || self.routing_options(entry)?.len() <= 1 {
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
    /// [`Self::backtrack_forced`]), then drop the selection. A
    /// best-effort pending is dropped outright, its loss is tolerated by
    /// design and must not burn backtracking budget.
    fn recover_doomed(&self, state: &mut PlanState, trail: &mut ForcedTrail) {
        let pending = state.pop_pending().unwrap();
        if pending.best_effort || !self.backtrack_forced(state, trail) {
            if !self.try_split_repush(state, &pending) {
                self.drop_unresolvable(state, &pending);
            }
        }
    }

    /// Pop the top pending selection and commit its best-ranked option.
    ///
    /// A failed commit rolls the whole state back to just after the pop:
    /// `commit_choice` pushes pendings mid-flight, and a graph-only rollback
    /// would leak entries whose `ordering_dependent` names a node index the
    /// rollback freed (StableDiGraph reuses indices). A failure first tries
    /// the pending's own lower-ranked options and then ancestor frames via
    /// [`Self::backtrack_forced`]; only when nothing recovers is the drop
    /// counted and penalized by the cost function.
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
                direct = options[0].is_direct(),
                error = ?e,
                "forced commit failed, backtracking",
            );
        }
        let failed = result.is_err();
        let best_effort = pending.best_effort;
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
        if failed && !best_effort && !self.backtrack_forced(state, trail) {
            if !self.try_split_repush(state, &pending) {
                state.dropped_fields += 1;
            }
        }
    }

    /// Rewind the forced-commit trail after a drop and try alternatives,
    /// deepest frame first. Returns `true` when the state was rewound to a
    /// committed alternative. Returns `false` when nothing was attempted
    /// (empty trail or budget spent).
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
        // Re-drive the greedy choice so the caller resumes from a
        // committed state; descendants that drop again find the budget
        // spent and fall through to plain drops.
        if let Some((pending, choice)) = parked {
            if self.commit_choice(state, &pending, &choice).is_err() {
                state.dropped_fields += 1;
            }
            return true;
        }
        false
    }
}

/// Short human-readable label for a selection, for logging.
pub(super) fn selection_label(selection: &Selection) -> String {
    match selection {
        Selection::Field(f) => f.field.field_position.to_string(),
        Selection::InlineFragment(f) => match &f.inline_fragment.type_condition_position {
            Some(type_cond) => format!("... on {}", type_cond.type_name()),
            None => "...".to_string(),
        },
    }
}

/// Upper bound on forced-commit backtracking attempts per candidate. Each
/// attempt is one rollback + commit; the cap keeps unplannable operations
/// from spending unbounded time in the budget-free greedy pass.
const FORCED_BACKTRACK_CAP: u64 = 256;

/// One multi-option forced commit on the fast-forward path, kept so a later
/// drop can rewind to it and try the next-ranked option.
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
    doomed: HashSet<(NodeIndex, PendingSiteKey)>,
}

/// Identity of a pending's routing position: the query graph node plus the
/// selection's field or type-condition name. Coarser than the full routing
/// cache key, but sufficient for fail-fast in forced backtracking.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PendingSiteKey {
    Field(Name),
    InlineFragment(Option<Name>),
}

fn pending_site(pending: &PendingSelection) -> (NodeIndex, PendingSiteKey) {
    let key = match &pending.selection {
        Selection::Field(f) => PendingSiteKey::Field(f.field.name().clone()),
        Selection::InlineFragment(f) => PendingSiteKey::InlineFragment(
            f.inline_fragment
                .type_condition_position
                .as_ref()
                .map(|pos| pos.type_name().clone()),
        ),
    };
    (pending.query_graph_node, key)
}

/// Wrap child selections back into a parent selection's shape.
fn wrap_in_parent(parent: &Selection, children: &[Selection]) -> Option<Selection> {
    let template = parent.selection_set()?;
    let mut map = crate::operation::SelectionMap::new();
    for child in children {
        let child = child
            .rebase_on(&template.type_position, &template.schema)
            .ok()?;
        map.insert(child);
    }
    let wrapped_ss = SelectionSet {
        schema: template.schema.clone(),
        type_position: template.type_position.clone(),
        selections: Arc::new(map),
    };
    Some(match parent {
        Selection::Field(field_sel) => {
            Selection::Field(Arc::new(crate::operation::FieldSelection {
                field: field_sel.field.clone(),
                selection_set: Some(wrapped_ss),
            }))
        }
        Selection::InlineFragment(frag_sel) => {
            Selection::InlineFragment(Arc::new(crate::operation::InlineFragmentSelection {
                inline_fragment: frag_sel.inline_fragment.clone(),
                selection_set: wrapped_ss,
            }))
        }
    })
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

    /// Apply a routing choice to the candidate in place: pops the decision
    /// and commits the choice.
    fn apply(
        &self,
        candidate: &mut PlanState,
        decision: &Arc<PendingSelection>,
        choice: &RoutingChoice,
    ) {
        // A choice is a unit of effort even when the commit pushes no
        // children (leaf fields); otherwise flat operations register no
        // effort and the search's fuel budget never binds.
        candidate.effort += 1;
        if matches!(
            choice,
            RoutingChoice::TypeExplosion | RoutingChoice::StripFragment
        ) {
            candidate.type_explosions += 1;
        }
        let Some(pending) = candidate.pop_pending() else {
            return;
        };
        debug_assert!(
            Arc::ptr_eq(&pending, decision),
            "apply must pop the decision that advance returned",
        );

        trace!(
            selection = %selection_label(&pending.selection),
            target_subgraph = %choice.target_subgraph(),
            choice = ?choice,
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
            if !self.try_split_repush(candidate, &pending) {
                candidate.dropped_fields += 1;
            }
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
        candidate.snapshot()
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
        // Dropped fields are hard failures (requested data omitted),
        // penalized so heavily that any complete plan beats them.
        // Type explosions defer their real fetch cost to child fragments,
        // so the probe (apply → cost → rollback) sees them as free;
        // the penalty ranks them above any structural cost but below
        // drops so BULB treats them as a last resort.
        let cost =
            base + candidate.type_explosions as f64 * 5e17 + candidate.dropped_fields as f64 * 1e18;
        trace!(cost, "candidate cost");
        cost
    }
}

#[cfg(test)]
mod tests;
