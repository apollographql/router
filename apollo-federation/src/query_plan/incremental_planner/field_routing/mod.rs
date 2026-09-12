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

mod commit;
mod conditions;
mod requires;
mod routing;
pub(super) mod state;
#[cfg(test)]
mod test_support;

use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::HashMap;
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

/// Type position and schema at a query graph node.
pub(super) struct NodeSource {
    pub(super) type_pos: CompositeTypeDefinitionPosition,
    pub(super) schema: ValidFederationSchema,
}

/// Search space presenting field-level routing decisions as a BULB problem.
pub(crate) struct FieldRoutingSearchSpace {
    pub(crate) query_graph: Arc<QueryGraph>,
    pub(crate) supergraph_schema: ValidFederationSchema,
    pub(crate) override_conditions: OverrideConditions,
    /// Subgraphs the caller disabled: enumeration never routes into them.
    pub(crate) disabled_subgraphs: apollo_compiler::collections::IndexSet<Arc<str>>,
}

impl FieldRoutingSearchSpace {
    pub(super) fn node_source(&self, node: NodeIndex) -> Result<NodeSource, FederationError> {
        let data = self.query_graph.node_weight(node)?;
        Ok(NodeSource {
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
    /// the schema check rejects @external fields, but they may still
    /// resolve at `node` when it is a provides-copy created by an
    /// ancestor's @provides, which only the query graph knows about.
    pub(super) fn can_resolve_in_place(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Result<bool, FederationError> {
        let satisfiable = self.can_satisfy(conditions, &source.type_pos, &source.schema)
            || self.conditions_resolvable_at_node(node, conditions)?;
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
    /// single-option selections greedily, drop zero-option ones, and lift
    /// forced entries above open decisions so their fetch groups inform
    /// scoring. Stops at the first multi-option selection (conditions
    /// included: which subgraph serves a key or requires field set is a
    /// decision like any other). A failed commit drops the selection; the
    /// search's discrepancy iterations explore the routings that avoid the
    /// failure, running unbudgeted until a complete plan exists.
    fn fast_forward(&self, state: &mut PlanState) -> Result<(), FederationError> {
        // Routing options are state-independent, so within this call they
        // are memoized per pending instance: the lift scan below would
        // otherwise re-enumerate every entry under the top once per lift.
        // Keyed by Arc address; the value keeps the Arc alive so addresses
        // cannot be recycled within the memo's lifetime.
        let mut memo: OptionsMemo = HashMap::new();
        while let Some(top) = state.pending.last() {
            let options = self.memoized_options(&mut memo, top)?;
            match options.len() {
                0 => {
                    if let Some(pending) = state.pop_pending() {
                        self.drop_unresolvable(state, &pending);
                    }
                }
                1 => self.commit_single(state, &options[0]),
                _ => {
                    // A decision point. Before stopping, commit any forced
                    // pendings deeper in the stack so their fetch groups
                    // inform this decision's scoring.
                    let mut lifted = false;
                    for index in (0..state.pending.len().saturating_sub(1)).rev() {
                        if self
                            .memoized_options(&mut memo, &state.pending[index])?
                            .len()
                            <= 1
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

    /// Look up (or compute and record) the routing options for a pending
    /// instance within one `fast_forward` call.
    fn memoized_options(
        &self,
        memo: &mut OptionsMemo,
        pending: &Arc<PendingSelection>,
    ) -> Result<Arc<Vec<RoutingChoice>>, FederationError> {
        let key = Arc::as_ptr(pending);
        if let Some((_, cached)) = memo.get(&key) {
            return Ok(cached.clone());
        }
        let computed = Arc::new(self.routing_options(pending)?);
        memo.insert(key, (pending.clone(), computed.clone()));
        Ok(computed)
    }

    /// Pop the top pending and commit its only option. A failed commit
    /// rolls back to just after the pop and counts the drop: commit_choice
    /// pushes pendings mid-flight, and a graph-only rollback would leak
    /// entries whose ordering dependent names a freed node index.
    fn commit_single(&self, state: &mut PlanState, choice: &RoutingChoice) {
        let Some(pending) = state.pop_pending() else {
            return;
        };
        let checkpoint = state.checkpoint();
        if let Err(e) = self.commit_choice(state, &pending, choice) {
            state.rollback(checkpoint);
            debug!(
                selection = %selection_label(&pending.selection),
                subgraph = %choice.target_subgraph(),
                error = ?e,
                "single-option commit failed, dropping",
            );
            state.dropped_fields += 1;
        }
    }
}

/// Per-call routing-options memo; the Arc in the value pins the pending so
/// its address (the key) cannot be recycled while the memo lives.
type OptionsMemo =
    HashMap<*const PendingSelection, (Arc<PendingSelection>, Arc<Vec<RoutingChoice>>)>;

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
        let cost = base + candidate.dropped_fields as f64 * 1e18;
        trace!(cost, "candidate cost");
        cost
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::state::ConditionScope;
    use super::test_support;
    use super::*;

    fn search_space() -> FieldRoutingSearchSpace {
        test_support::search_space(&[
            (
                "S1",
                r#"
                type Query { t: T }
                type T @key(fields: "k") { k: ID, x: Int }
                "#,
            ),
            (
                "S2",
                r#"
                type T @key(fields: "k") { k: ID, y: Int }
                "#,
            ),
        ])
    }

    fn t_node(space: &FieldRoutingSearchSpace, subgraph: &str) -> NodeIndex {
        test_support::node_for(space, subgraph, "T")
    }

    /// A pending for `T.y` (resolvable only in S2) anchored at fetch group
    /// `fetch_node`, marked as condition data feeding `dependent`.
    fn y_pending(
        space: &FieldRoutingSearchSpace,
        fetch_node: NodeIndex,
        dependent: Option<NodeIndex>,
    ) -> PendingSelection {
        let op = crate::operation::Operation::parse(
            space.supergraph_schema.clone(),
            r#"{ t { y } }"#,
            "op.graphql",
        )
        .expect("valid operation");
        let Some(Selection::Field(t_sel)) = op.selection_set.selections.values().next() else {
            panic!("expected t field");
        };
        let y_sel = t_sel
            .selection_set
            .as_ref()
            .expect("t has sub-selections")
            .selections
            .values()
            .next()
            .expect("y selection")
            .clone();
        PendingSelection {
            selection: y_sel,
            query_graph_node: t_node(space, "S1"),
            fetch_node,
            op_path: SharedPath::new(),
            path_in_fetch: SharedPath::new(),
            condition: dependent.map(|dependent| ConditionScope {
                dependent,
                depth: 1,
            }),
        }
    }

    /// State with root group A feeding entity group B, so an ordering
    /// dependent of A cycles when a new group hangs beneath B.
    fn cyclic_fixture() -> (PlanState, NodeIndex, NodeIndex) {
        let mut state = PlanState::new(vec![]);
        let s1: Arc<str> = Arc::from("S1");
        let root_pos: CompositeTypeDefinitionPosition = CompositeTypeDefinitionPosition::Object(
            crate::schema::position::ObjectTypeDefinitionPosition {
                type_name: name!("Query"),
            },
        );
        let a = state.graph.get_or_create_root_group(&s1, root_pos);
        let b = state.graph.add_entity_group(&s1, vec![]);
        state.graph.add_dependency(a, b, vec![]);
        (state, a, b)
    }

    /// Reusing an entity group that already (transitively) feeds the anchor
    /// would close a dependency cycle; the commit must mint a fresh group
    /// instead. This arm is the release-mode acyclicity guard.
    #[test]
    fn cyclic_entity_group_reuse_mints_fresh_group() {
        let space = search_space();
        let (mut state, _a, b) = cyclic_fixture();
        let s2: Arc<str> = Arc::from("S2");
        // An existing (S2, []) entity group that already feeds b: reusing
        // it for a hop anchored at b would close a cycle.
        let existing = state.graph.get_or_create_entity_group(&s2, vec![]);
        state.graph.add_dependency(existing, b, vec![]);

        let pending = Arc::new(y_pending(&space, b, None));
        let options = Arc::new(space.routing_options(&pending).expect("options enumerate"));
        assert!(!options.is_empty(), "y must have a key-hop option");

        space
            .commit_choice(&mut state, &pending, &options[0])
            .expect("commit mints a fresh group instead of reusing");
        assert!(
            !state.graph.has_edge(b, existing),
            "reuse would have closed a cycle",
        );
        // cost() debug-asserts acyclicity; finite means the graph is a DAG.
        assert!(state.graph.cost().is_finite());
    }

    /// A failed commit drops only its own selection and rolls its
    /// mutations back; the same site under a different anchor commits.
    #[test]
    fn failed_commit_drops_only_the_failed_pending() {
        let space = search_space();
        let (mut state, a, b) = cyclic_fixture();

        // Bottom of stack: same site, no ordering dependent (commits fine).
        // Top: condition pending whose ordering edge cycles (commit fails).
        let ok_pending = y_pending(&space, b, None);
        let cyclic_pending = y_pending(&space, b, Some(a));
        state.pending = vec![Arc::new(ok_pending), Arc::new(cyclic_pending)];

        space.fast_forward(&mut state).expect("fast forward runs");

        assert_eq!(
            state.dropped_fields, 1,
            "only the cyclic pending drops; the same site with a different anchor commits",
        );
        assert!(state.pending.is_empty());
    }
}
