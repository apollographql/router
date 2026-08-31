//! Field-level routing as a BULB search space.
//!
//! The planner walks the operation selection-by-selection ("pendings"),
//! consulting the query graph for where each field can be resolved:
//! - [`cached_query_graph`]: memoizing wrapper for immutable QueryGraph lookups.
//! - [`state`]: mutable search state (pending stack, checkpoints).
//! - [`routing`]: enumerating and ranking options for a selection.
//! - [`commit`]: applying a chosen option to the fetch graph.
//! - [`conditions`]: condition satisfiability for @requires / @key.
//! - [`requires`]: hop-edge inputs and condition paths.
//!
//! This file holds the search-space type, its caches, and the
//! [`BulbSearchSpace`] implementation.

pub(super) mod cached_query_graph;
mod commit;
mod conditions;
mod context;
mod requires;
mod routing;
pub(super) mod state;
mod type_conditions;

use std::cell::RefCell;
use std::sync::Arc;

use apollo_compiler::Name;
use cached_query_graph::CachedQueryGraph;
use hashbrown::HashMap;
use hashbrown::HashSet;
use petgraph::graph::NodeIndex;
use routing::RoutingChoice;
use routing::RoutingTarget;
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

/// Site key for routing dedup and the doomed-site set.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RoutingSiteKey {
    Field(Name),
    InlineFragment(Option<Name>),
}

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
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum SelectionArcKey {
    Field(ArcKey<crate::operation::FieldSelection>),
    InlineFragment(ArcKey<crate::operation::InlineFragmentSelection>),
}

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

/// Subgraph, type position, and schema at a query graph node.
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
    pub(super) disabled_subgraphs: apollo_compiler::collections::IndexSet<Arc<str>>,
}

impl FieldRoutingSearchSpace {
    pub(super) fn node_source(&self, node: NodeIndex) -> Result<NodeSource, FederationError> {
        let data = self.cached_query_graph.query_graph.node_weight(node)?;
        Ok(NodeSource {
            subgraph: data.source.clone(),
            type_pos: data.type_.clone().try_into()?,
            schema: self
                .cached_query_graph
                .query_graph
                .schema_by_source(&data.source)?
                .clone(),
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

    /// Op path at which selections enter an entity fetch group.
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
    pub(super) fn can_resolve_in_place(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Result<bool, FederationError> {
        let satisfiable = self.cached_can_satisfy(
            conditions,
            &source.type_pos,
            &source.schema,
        ) || self.conditions_resolvable_at_node(node, conditions.as_ref())?;
        Ok(satisfiable && !self.conditions_have_requires(node, conditions)?)
    }

    /// Advance through everything that is not a genuine decision: commit
    /// single-option selections and condition pendings greedily, handle
    /// zero-option selections via drops, and lift forced entries above open
    /// decisions so their fetch groups inform scoring. Stops at the first
    /// multi-option ordinary selection.
    fn fast_forward(&self, state: &mut PlanState) -> Result<(), FederationError> {
        let mut trail = ForcedTrail::default();
        while let Some(top) = state.pending.last() {
            if !trail.doomed.is_empty() && trail.doomed.contains(&pending_site(top)) {
                self.recover_doomed(state, &mut trail);
                continue;
            }
            let options = self.cached_routing_options(top)?;
            match options.len() {
                0 => {
                    trail.doomed.insert(pending_site(top));
                    self.recover_doomed(state, &mut trail);
                }
                1 => self.commit_forced(state, options, &mut trail),
                _ if top.condition.is_some() => self.commit_forced(state, options, &mut trail),
                _ => {
                    let mut lifted = false;
                    for index in (0..state.pending.len().saturating_sub(1)).rev() {
                        let entry = &state.pending[index];
                        if entry.condition.is_some()
                            || self.cached_routing_options(entry)?.len() <= 1
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

    /// Pop a pending whose site is proven hopeless and recover.
    fn recover_doomed(&self, state: &mut PlanState, trail: &mut ForcedTrail) {
        let pending = state.pop_pending().unwrap();
        if pending.best_effort || !self.backtrack_forced(state, trail) {
            self.drop_unresolvable(state, &pending);
        }
    }

    /// Pop the top pending selection and commit its best-ranked option.
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
            state.dropped_fields += 1;
        }
    }

    /// Rewind the forced-commit trail after a drop and try alternatives.
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
        if let Some((pending, choice)) = parked {
            if self.commit_choice(state, &pending, &choice).is_err() {
                state.dropped_fields += 1;
            }
            return true;
        }
        false
    }
}

/// One multi-option forced commit on the fast-forward path.
struct ForcedFrame {
    pending: Arc<PendingSelection>,
    options: Arc<Vec<RoutingChoice>>,
    next_option: usize,
    checkpoint: PlanCheckpoint,
}

/// Backtracking state for a single `fast_forward` call.
#[derive(Default)]
struct ForcedTrail {
    frames: Vec<ForcedFrame>,
    doomed: HashSet<(NodeIndex, RoutingSiteKey)>,
}

/// Identity of a pending's routing position.
fn pending_site(pending: &PendingSelection) -> (NodeIndex, RoutingSiteKey) {
    let key = match &pending.selection {
        Selection::Field(f) => RoutingSiteKey::Field(f.field.name().clone()),
        Selection::InlineFragment(f) => RoutingSiteKey::InlineFragment(
            f.inline_fragment
                .type_condition_position
                .as_ref()
                .map(|pos| pos.type_name().clone()),
        ),
    };
    (pending.query_graph_node, key)
}

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
    type Decision = Arc<PendingSelection>;
    type Choice = RoutingChoice;
    type Checkpoint = PlanCheckpoint;

    fn advance(&self, candidate: &mut PlanState) -> AdvanceResult<Arc<PendingSelection>> {
        if let Err(e) = self.fast_forward(candidate) {
            debug!(error = %e, "fast_forward error, completing candidate early");
            candidate.dropped_fields += candidate.pending.len().max(1);
            while candidate.pop_pending().is_some() {}
            return AdvanceResult::Complete;
        }
        match candidate.pending.last().cloned() {
            Some(decision) => AdvanceResult::Decision(decision),
            None => AdvanceResult::Complete,
        }
    }

    fn options(&self, decision: &Arc<PendingSelection>) -> Vec<RoutingChoice> {
        self.routing_options(decision).unwrap_or_default()
    }

    fn apply(
        &self,
        candidate: &mut PlanState,
        _decision: &Arc<PendingSelection>,
        choice: &RoutingChoice,
    ) {
        candidate.effort += 1;
        if matches!(
            choice.target,
            RoutingTarget::TypeExplosion | RoutingTarget::RestructureFragment
        ) {
            candidate.type_explosions += 1;
        }
        let Some(pending) = candidate.pop_pending() else {
            return;
        };

        trace!(
            selection = %selection_label(&pending.selection),
            target_subgraph = %choice.target_subgraph(),
            hop_kind = ?choice.hop_kind,
            "applying routing choice",
        );

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

    fn snapshot(&self, candidate: &PlanState) -> PlanState {
        candidate.clone()
    }

    fn is_complete(&self, candidate: &PlanState) -> bool {
        candidate.dropped_fields == 0 && candidate.pending.is_empty()
    }

    fn effort(&self, candidate: &PlanState) -> u64 {
        candidate.effort
    }

    fn cost(&self, candidate: &PlanState) -> QueryPlanCost {
        let base = candidate.graph.cost();
        // Dropped fields are hard failures (requested data omitted),
        // penalized so heavily that any complete plan beats them.
        // Type explosions defer their real fetch cost to child fragments,
        // so the probe (apply → cost → rollback) sees them as free;
        // the penalty ranks them above any structural cost but below
        // drops so BULB treats them as a last resort.
        let cost = base
            + candidate.type_explosions as f64 * 5e17
            + candidate.dropped_fields as f64 * 1e18;
        trace!(cost, "candidate cost");
        cost
    }
}

#[cfg(test)]
mod tests;
