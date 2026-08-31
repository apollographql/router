//! Mutable BULB search state: the pending-selection stack, the fetch graph
//! under construction, and O(1) checkpoint/rollback over both.

use std::collections::BTreeMap;
use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::super::fetch_graph::FetchGraph;
use super::super::shared_path::SharedPath;
use crate::operation::Selection;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// Condition bookkeeping for a pending selection that carries a
/// @requires / @key field set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConditionScope {
    /// The fetch group consuming the condition data through its entity
    /// representation. Every group the condition selection (or its children)
    /// commits into gets an ordering edge to this dependent.
    pub(crate) dependent: NodeIndex,
    /// Condition-resolution nesting level. Bounds requires-of-requires
    /// chains: mutually recursive @requires would otherwise spiral forever,
    /// each round minting fresh entity groups.
    pub(crate) depth: u8,
}

/// Anchor information for @fromContext across entity boundaries.
#[derive(Clone, Debug, Default)]
pub(crate) struct ContextAnchor {
    /// The *parent* fetch feeding this selection's entity fetch, when the
    /// selection lives inside one. When the ancestor with @context is at or
    /// above the entity boundary, the context selection must be added here,
    /// not to the entity fetch.
    pub(crate) fetch: Option<NodeIndex>,
    /// Op path at the entity boundary in the parent fetch.
    pub(crate) op_path: SharedPath<Arc<OpPathElement>>,
    /// Entity root type at the boundary. When the ancestor with @context
    /// matches it, context data rides the entity representation without a
    /// new hop.
    pub(crate) entity_type: Option<CompositeTypeDefinitionPosition>,
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
    /// @provides provenance across downcasts: the provides-copy query graph
    /// node this position descended from via inline fragments, when the
    /// current node itself is not a copy. An ancestor's `@provides` on an
    /// interface-typed field applies to every runtime type, but the query
    /// graph only copies the nodes named in the provides field set. A
    /// downcast out of the copy layer lands on the original node, where the
    /// provided fields have no edges. The anchor keeps the copy node (whose
    /// edges are the provided fields) visible to key-hop enumeration, so
    /// "are these key conditions provided here?" stays an exact graph check
    /// instead of a schema-level guess. `None` whenever the current node's
    /// own edges carry the provenance (inside a copy layer) or no @provides
    /// is in scope.
    pub(crate) provides_anchor: Option<NodeIndex>,
    /// The @defer label this selection is inside, if any. Propagated to
    /// fetch nodes so they can be partitioned into primary vs deferred.
    pub(crate) defer_ref: Option<String>,
    /// Type spine from the operation root through parents of this selection,
    /// for @fromContext ancestor resolution.
    pub(crate) parent_types: SharedPath<CompositeTypeDefinitionPosition>,
    /// @fromContext anchor: the parent fetch feeding this selection's
    /// entity fetch, when the selection lives inside one.
    pub(crate) context_anchor: ContextAnchor,
    /// Best-effort selection: dropping it (zero routing options, or a failed
    /// commit) is tolerated silently instead of counting toward
    /// `dropped_fields` and failing the plan. Inherited by forks, so
    /// condition data pushed on a best-effort selection's behalf is equally
    /// tolerant. The only producer is the @interfaceObject
    /// concrete-`__typename` recovery, where no subgraph may be able to
    /// supply the concrete typename.
    pub(crate) best_effort: bool,
    /// Ancestor pending whose routing position offers alternatives when this
    /// selection is stranded. The chain is set by `dispatch_sub_selections`
    /// so `try_split_repush` can walk up to find a field that can be routed
    /// to a different subgraph.
    pub(crate) split_parent: Option<Arc<PendingSelection>>,
    /// When set, `cached_routing_options` filters out options targeting this
    /// subgraph, preventing the re-pushed remainder from looping back to the
    /// subgraph that stranded it.
    pub(crate) split_avoid: Option<Arc<str>>,
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
            provides_anchor: self.provides_anchor,
            defer_ref: self.defer_ref.clone(),
            parent_types: self.parent_types.clone(),
            context_anchor: self.context_anchor.clone(),
            best_effort: self.best_effort,
            split_parent: self.split_parent.clone(),
            split_avoid: self.split_avoid.clone(),
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

    pub(super) fn with_provides_anchor(mut self, provides_anchor: Option<NodeIndex>) -> Self {
        self.provides_anchor = provides_anchor;
        self
    }

    pub(super) fn with_defer(mut self, defer_ref: Option<String>) -> Self {
        self.defer_ref = defer_ref;
        self
    }

    pub(super) fn with_parent_types(
        mut self,
        parent_types: SharedPath<CompositeTypeDefinitionPosition>,
    ) -> Self {
        self.parent_types = parent_types;
        self
    }

    pub(super) fn with_context_anchor(mut self, context_anchor: ContextAnchor) -> Self {
        self.context_anchor = context_anchor;
        self
    }

    /// Mark this selection best-effort: a drop is tolerated silently (see
    /// [`Self::best_effort`]).
    pub(super) fn into_best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }

    pub(super) fn with_split_parent(mut self, parent: Option<Arc<PendingSelection>>) -> Self {
        self.split_parent = parent;
        self
    }

    pub(super) fn with_split_avoid(mut self, avoid: Option<Arc<str>>) -> Self {
        self.split_avoid = avoid;
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
    pub(super) fn condition_depth(&self) -> u8 {
        self.condition.map_or(0, |c| c.depth)
    }
}

/// Undo-log entry for one pending-stack mutation.
#[derive(Clone)]
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
/// cloning. `Clone` is only used for `snapshot()` (saving the best complete
/// candidate) and is cheap: both stacks hold `Arc`s.
#[derive(Clone)]
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
    /// Type-explosion or fragment-restructuring commits applied. These
    /// decompose abstract types into per-concrete-type fragments, deferring
    /// real fetch cost to later decisions. The probe (apply → cost →
    /// rollback) sees them as free, so without a penalty BULB chases them
    /// eagerly, creating combinatorial blowup on wide interfaces. Penalized
    /// in `cost()` above any structural cost but below drops.
    pub(crate) type_explosions: usize,
    /// Monotonic count of pending-stack pushes over the whole search,
    /// including rolled-back work. Every unit of planning effort flows
    /// through `push_pending`, so this tracks wall time far more tightly
    /// than decision counts. Used by the search's effort budget;
    /// deliberately not restored by `rollback`.
    pub(crate) effort: u64,
    /// Monotonic count of forced-commit backtracking attempts (see
    /// `backtrack_forced`). Like `effort`, deliberately not restored by
    /// `rollback`: the cap must bound total work even when the greedy pass
    /// (which has no effort budget) keeps hitting doomed forced commits.
    pub(crate) forced_backtracks: u64,
    /// Interned ids for @requires condition-field aliases, keyed by the
    /// serialized condition selection: identical conditions share an alias
    /// so sibling entity fetches staging the same @requires can merge;
    /// distinct conditions get distinct aliases. Append-only; not restored
    /// on rollback (aliases only need to be stable, not predictable).
    pub(crate) condition_alias_ids: BTreeMap<String, usize>,
    /// Count of split re-pushes. Penalized in cost() at 1e15 (below the
    /// 1e18 drop penalty, above structural cost). Saved/restored by
    /// checkpoint/rollback.
    pub(crate) splits: usize,
    /// Off in first search pass. Enabled in retry after a split-free search
    /// fails with drops. Constant for a search, so checkpoints ignore it.
    pub(crate) split_repush_enabled: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PlanCheckpoint {
    graph_cp: usize,
    pending_cp: usize,
    dropped_fields: usize,
    splits: usize,
    type_explosions: usize,
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
            type_explosions: 0,
            effort: 0,
            forced_backtracks: 0,
            condition_alias_ids: BTreeMap::new(),
            splits: 0,
            split_repush_enabled: false,
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

    /// Save the current state for later rollback. O(1).
    pub(crate) fn checkpoint(&self) -> PlanCheckpoint {
        PlanCheckpoint {
            graph_cp: self.graph.checkpoint(),
            pending_cp: self.pending_undo.len(),
            dropped_fields: self.dropped_fields,
            splits: self.splits,
            type_explosions: self.type_explosions,
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
        self.splits = cp.splits;
        self.type_explosions = cp.type_explosions;
    }
}
