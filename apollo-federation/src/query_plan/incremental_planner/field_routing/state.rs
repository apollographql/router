//! Mutable BULB search state: the pending-selection stack, the fetch graph
//! under construction, and O(1) checkpoint/rollback over both.

use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::super::fetch_graph::FetchGraph;
use super::super::fetch_graph::FetchGraphCheckpoint;
use super::super::shared_path::SharedPath;
use crate::operation::Selection;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;

/// Condition bookkeeping for a pending selection that carries a
/// @requires / @key field set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConditionScope {
    /// The fetch group consuming the condition data through its entity
    /// representation. Every group the condition selection (or its children)
    /// commits into gets an ordering edge to this dependent.
    pub(crate) dependent: NodeIndex,
    /// Condition-resolution nesting level. Bounds requires-of-requires
    /// chains: mutually recursive @requires would otherwise spiral forever.
    pub(crate) depth: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingSelection {
    /// The field or inline fragment to resolve.
    pub(crate) selection: Selection,
    /// Current position in the QueryGraph (determines which subgraph we're in).
    pub(crate) query_graph_node: NodeIndex,
    /// Which fetch graph node to add this field to.
    pub(crate) fetch_node: NodeIndex,
    /// Operation path from the fetch node's root to this selection's parent.
    pub(crate) op_path: SharedPath<Arc<OpPathElement>>,
    /// Response path within the fetch node, for result merging.
    pub(crate) path_in_fetch: SharedPath<FetchDataPathElement>,
    /// Set when this selection is a condition (@requires / @key field set);
    /// `None` for ordinary query selections.
    pub(crate) condition: Option<ConditionScope>,
}

impl PendingSelection {
    /// A selection at the same routing position as `self`; chain `with_*`
    /// builders to move any part of it.
    pub(super) fn fork(&self, selection: Selection) -> Self {
        Self {
            selection,
            query_graph_node: self.query_graph_node,
            fetch_node: self.fetch_node,
            op_path: self.op_path.clone(),
            path_in_fetch: self.path_in_fetch.clone(),
            condition: self.condition,
        }
    }

    pub(super) fn at(mut self, query_graph_node: NodeIndex, fetch_node: NodeIndex) -> Self {
        self.query_graph_node = query_graph_node;
        self.fetch_node = fetch_node;
        self
    }

    pub(super) fn with_op_path(mut self, op_path: SharedPath<Arc<OpPathElement>>) -> Self {
        self.op_path = op_path;
        self
    }

    pub(super) fn with_response_path(
        mut self,
        path_in_fetch: SharedPath<FetchDataPathElement>,
    ) -> Self {
        self.path_in_fetch = path_in_fetch;
        self
    }

    /// Mark this selection as condition data feeding `dependent`'s entity
    /// representation, one nesting level deeper than its anchor.
    pub(super) fn into_condition_for(mut self, dependent: NodeIndex) -> Self {
        self.condition = Some(ConditionScope {
            dependent,
            depth: self.condition_depth() + 1,
        });
        self
    }

    /// The fetch group consuming this selection's condition data, if it is
    /// a condition.
    pub(crate) fn ordering_dependent(&self) -> Option<NodeIndex> {
        self.condition.map(|c| c.dependent)
    }

    /// Condition nesting level; 0 for ordinary query selections.
    pub(super) fn condition_depth(&self) -> usize {
        self.condition.map_or(0, |c| c.depth)
    }
}

/// Undo-log entry for one pending-stack mutation.
enum PendingOp {
    /// An entry was pushed. Undo: pop and drop it.
    Pushed,
    /// An entry was popped. Undo: push it back. Holding the `Arc` keeps
    /// this O(1) (no deep clone of the selection).
    Popped(Arc<PendingSelection>),
    /// A mid-stack entry was lifted to the top (most-constrained-first
    /// ordering). Undo: pop the top and reinsert at its original index.
    Lifted(usize),
}

/// BULB state: a partially-built query plan.
///
/// A single `PlanState` is mutated during search; trial branches are
/// applied, scored, and undone via `checkpoint()` / `rollback()` without
/// cloning. `snapshot()` saves the best complete candidate.
pub(crate) struct PlanState {
    /// Lightweight fetch graph tracking groups, dependencies, and selections.
    pub(crate) graph: FetchGraph,
    /// Fields/fragments not yet routed to a subgraph. Mutate only through
    /// `push_pending` / `pop_pending` so the undo log stays consistent.
    pub(crate) pending: Vec<Arc<PendingSelection>>,
    /// Undo log for `pending`. Checkpoints record its length; rollback
    /// replays entries in reverse.
    pending_undo: Vec<PendingOp>,
    /// Fields dropped for lack of routing options. Heavily penalized in
    /// `cost()` so BULB backtracks to explore alternatives.
    pub(crate) dropped_fields: usize,
    /// Monotonic count of pending-stack pushes over the whole search,
    /// including rolled-back work. Every unit of planning effort flows
    /// through `push_pending`, so this tracks wall time far more tightly
    /// than decision counts. Used by the search's effort budget;
    /// deliberately not restored by `rollback`.
    pub(crate) effort: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PlanCheckpoint {
    graph_cp: FetchGraphCheckpoint,
    pending_cp: usize,
    dropped_fields: usize,
}

impl PlanState {
    pub(crate) fn new(pending: Vec<PendingSelection>) -> Self {
        Self::with_graph(FetchGraph::new(), pending)
    }

    pub(crate) fn with_graph(graph: FetchGraph, pending: Vec<PendingSelection>) -> Self {
        Self {
            graph,
            pending: pending.into_iter().map(Arc::new).collect(),
            pending_undo: Vec::new(),
            dropped_fields: 0,
            effort: 0,
        }
    }

    /// Push a selection onto the pending stack, logging the mutation.
    pub(super) fn push_pending(&mut self, selection: PendingSelection) {
        self.effort += 1;
        self.pending.push(Arc::new(selection));
        self.pending_undo.push(PendingOp::Pushed);
    }

    /// Move the pending at `index` to the top of the stack (logged for
    /// rollback). Used to commit forced selections before open decisions,
    /// so their fetch groups inform the decision's scoring.
    pub(super) fn lift_pending(&mut self, index: usize) {
        let entry = self.pending.remove(index);
        self.pending.push(entry);
        self.pending_undo.push(PendingOp::Lifted(index));
    }

    /// Pop the top pending selection, logging the mutation. The returned
    /// `Arc` is shared with the undo log entry, so undo is O(1).
    pub(super) fn pop_pending(&mut self) -> Option<Arc<PendingSelection>> {
        let popped = self.pending.pop()?;
        self.pending_undo.push(PendingOp::Popped(popped.clone()));
        Some(popped)
    }

    /// Clone for saving a completed candidate; drops both undo logs, which
    /// snapshots never roll back.
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            graph: self.graph.snapshot(),
            pending: self.pending.clone(),
            pending_undo: Vec::new(),
            dropped_fields: self.dropped_fields,
            effort: self.effort,
        }
    }

    /// Save the current state for later rollback. O(1).
    pub(crate) fn checkpoint(&self) -> PlanCheckpoint {
        PlanCheckpoint {
            graph_cp: self.graph.checkpoint(),
            pending_cp: self.pending_undo.len(),
            dropped_fields: self.dropped_fields,
        }
    }

    /// Restore to a previously saved checkpoint, undoing all mutations since.
    pub(crate) fn rollback(&mut self, cp: PlanCheckpoint) {
        self.graph.rollback(cp.graph_cp);
        while self.pending_undo.len() > cp.pending_cp {
            match self.pending_undo.pop().unwrap() {
                PendingOp::Pushed => {
                    self.pending.pop();
                }
                PendingOp::Popped(entry) => {
                    self.pending.push(entry);
                }
                PendingOp::Lifted(index) => {
                    let entry = self.pending.pop().unwrap();
                    self.pending.insert(index, entry);
                }
            }
        }
        self.dropped_fields = cp.dropped_fields;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_selection() -> Selection {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            "type Query { x: Int }",
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");
        let op = crate::operation::Operation::parse(schema, "{ x }", "op.graphql")
            .expect("valid operation");
        op.selection_set
            .selections
            .values()
            .next()
            .expect("one selection")
            .clone()
    }

    /// Push/pop/lift interleavings across nested checkpoints must restore
    /// the stack exactly.
    #[test]
    fn pending_log_rolls_back_interleaved_mutations() {
        let entry = |n: u32| PendingSelection {
            selection: any_selection(),
            query_graph_node: NodeIndex::new(n as usize),
            fetch_node: NodeIndex::new(0),
            op_path: SharedPath::new(),
            path_in_fetch: SharedPath::new(),
            condition: None,
        };
        let ids = |state: &PlanState| -> Vec<usize> {
            state
                .pending
                .iter()
                .map(|p| p.query_graph_node.index())
                .collect()
        };

        let mut state = PlanState::new(vec![entry(0), entry(1), entry(2)]);
        let outer = state.checkpoint();
        state.push_pending(entry(3));
        state.lift_pending(1);
        let inner = state.checkpoint();
        state.pop_pending().expect("pops the lifted entry");
        state.lift_pending(0);
        state.push_pending(entry(4));
        assert_eq!(ids(&state), vec![2, 3, 0, 4]);

        state.rollback(inner);
        assert_eq!(ids(&state), vec![0, 2, 3, 1]);
        state.rollback(outer);
        assert_eq!(ids(&state), vec![0, 1, 2]);
    }
}
