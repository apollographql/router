//! The fetch graph: fetch groups (nodes), dependencies (edges), and the
//! entity inputs riding those edges, built incrementally during BULB
//! search with O(1) checkpoint / undo-log rollback.

#[allow(dead_code)]
pub(crate) mod plan_builder;
pub(crate) mod selection_builder;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::Direction;
use petgraph::stable_graph::EdgeIndex;
use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::EdgeRef;
use selection_builder::SelectionBuilder;
use selection_builder::SelectionCheckpoint;

use super::shared_path::SharedPath;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::QueryPlanCost;
use crate::schema::position::CompositeTypeDefinitionPosition;

pub(crate) const FETCH_COST: QueryPlanCost = 1000.0;
pub(crate) const PIPELINING_COST: QueryPlanCost = 100.0;

#[derive(Clone, Debug)]
pub(crate) enum FetchGroupKind {
    Root {
        root_type: CompositeTypeDefinitionPosition,
    },
    Entity {
        merge_at: Vec<FetchDataPathElement>,
    },
    RootHop {
        root_type: CompositeTypeDefinitionPosition,
        merge_at: Vec<FetchDataPathElement>,
    },
}

/// Deferred entity input: stored during search, applied when building
/// the query plan from the winning FetchGraph.
#[derive(Clone, Debug)]
pub(crate) struct InputContribution {
    pub(crate) source_type_name: Name,
    pub(crate) conditions: Arc<SelectionSet>,
    /// Present for key-hop inputs (triggers `compute_input_rewrites_on_key_fetch`);
    /// absent for @requires inputs.
    pub(crate) rewrite_info: Option<InputRewriteInfo>,
}

#[derive(Clone, Debug)]
pub(crate) struct InputRewriteInfo {
    pub(crate) dest_type: CompositeTypeDefinitionPosition,
    pub(crate) dest_subgraph: Arc<str>,
}

/// Node weight in the FetchGraph.
#[derive(Clone, Debug)]
pub(crate) struct FetchNode {
    pub(crate) subgraph: Arc<str>,
    pub(crate) kind: FetchGroupKind,
    pub(crate) selection_builder: SelectionBuilder,
}

impl FetchNode {
    pub(crate) fn new(subgraph: Arc<str>, kind: FetchGroupKind) -> Self {
        Self {
            subgraph,
            kind,
            selection_builder: SelectionBuilder::default(),
        }
    }

    /// Get the root type if this is a root fetch group.
    #[allow(dead_code)]
    pub(crate) fn root_type(&self) -> Option<&CompositeTypeDefinitionPosition> {
        match &self.kind {
            FetchGroupKind::Root { root_type } | FetchGroupKind::RootHop { root_type, .. } => {
                Some(root_type)
            }
            FetchGroupKind::Entity { .. } => None,
        }
    }
}

/// Edge weight, directed parent->child; inputs describe what the parent
/// must send to the child.
#[derive(Clone, Debug)]
pub(crate) struct FetchEdgeWeight {
    pub(crate) inputs: Vec<InputContribution>,
}

/// A single undoable mutation on the FetchGraph. Logged by mutating
/// methods and replayed in reverse by `rollback()`.
#[derive(Clone, Debug)]
enum FetchGraphOp {
    /// A node was added. Undo: remove_node (StableDiGraph keeps indices
    /// stable); a root node (`root_key` set) also removes its root_groups
    /// entry.
    AddNode {
        node_index: NodeIndex,
        root_key: Option<Arc<str>>,
    },
    /// An edge was added. Undo: remove_edge.
    AddEdge(EdgeIndex),
    /// An input was appended to an edge. Undo: pop last input.
    AppendEdgeInput(EdgeIndex),
    /// A selection was appended to a node. Undo: restore previous head pointer.
    ModifySelection {
        node_index: NodeIndex,
        prev_head: SelectionCheckpoint,
    },
    /// A node's depth was raised by an edge insertion. Undo: restore the
    /// previous depth and stage counts.
    RaiseDepth { node_index: NodeIndex, prev: u32 },
}

/// Index key for entity fetch group reuse.
type EntityGroupKey = (Arc<str>, Vec<FetchDataPathElement>);

/// Lightweight fetch graph for BULB search.
///
/// Undo works via an append-only mutation log: `checkpoint()` returns the
/// current log position, `rollback(cp)` reverses back to it. Trial
/// branches are applied, scored, and undone on a single instance, so no
/// cloning during search.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraph {
    graph: StableDiGraph<FetchNode, FetchEdgeWeight>,
    /// Root groups keyed by subgraph name.
    root_groups: HashMap<Arc<str>, NodeIndex>,
    /// First-created entity group per (subgraph, merge_at), so
    /// `get_or_create_entity_group` (run on every key hop) is a lookup
    /// instead of a node scan. Only the first node with a key claims the
    /// slot (earliest-index-wins); undo is LIFO, so a later duplicate can
    /// never outlive the slot owner. Stale after post-search sibling
    /// merging, which is fine since nothing creates groups after search.
    entity_groups: HashMap<EntityGroupKey, NodeIndex>,
    undo_log: Vec<FetchGraphOp>,
    /// Pipeline depth (longest parent chain) per node index, maintained
    /// incrementally so `cost()` (the hot path, run per routing option)
    /// is O(max_depth) instead of a full graph walk.
    depth: Vec<u32>,
    /// The stage_counts array tracks how many fetch groups exist at each
    /// pipeline depth, so cost() can compute the structural cost in
    /// O(max_depth) instead of walking all nodes. Each depth's count is
    /// maintained incrementally as nodes are added and depths are raised.
    stage_counts: Vec<u64>,
    /// Set by post-search mutations that bypass incremental depth
    /// maintenance (sibling merging relocates edges and removes nodes).
    /// `cost()` is search-only and debug-asserts this is unset.
    depth_dirty: bool,
}

impl FetchGraph {
    pub(crate) fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            root_groups: HashMap::new(),
            entity_groups: HashMap::new(),
            undo_log: Vec::new(),
            depth: Vec::new(),
            stage_counts: Vec::new(),
            depth_dirty: false,
        }
    }

    /// Record a freshly added node at depth 0.
    fn register_node_depth(&mut self, node: NodeIndex) {
        let i = node.index();
        if self.depth.len() <= i {
            self.depth.resize(i + 1, 0);
        }
        self.depth[i] = 0;
        self.bump_stage_count(0, 1);
    }

    fn bump_stage_count(&mut self, depth: u32, delta: i64) {
        let d = depth as usize;
        if self.stage_counts.len() <= d {
            self.stage_counts.resize(d + 1, 0);
        }
        self.stage_counts[d] = self.stage_counts[d].wrapping_add_signed(delta);
    }

    /// Raise `node`'s depth to at least `min_depth`, propagating to its
    /// descendants. Each change is logged for rollback.
    fn raise_depth(&mut self, node: NodeIndex, min_depth: u32) {
        let prev = self.depth[node.index()];
        if prev >= min_depth {
            return;
        }
        self.undo_log.push(FetchGraphOp::RaiseDepth {
            node_index: node,
            prev,
        });
        self.bump_stage_count(prev, -1);
        self.bump_stage_count(min_depth, 1);
        self.depth[node.index()] = min_depth;
        let children: Vec<NodeIndex> = self
            .graph
            .edges_directed(node, Direction::Outgoing)
            .map(|e| e.target())
            .collect();
        for child in children {
            self.raise_depth(child, min_depth + 1);
        }
    }

    /// Current undo log position, for `rollback()`.
    pub(crate) fn checkpoint(&self) -> usize {
        self.undo_log.len()
    }

    /// Undo all mutations back to the given checkpoint position.
    ///
    /// Entries are replayed in reverse. `StableDiGraph::remove_node` is
    /// safe here because edges to added nodes are always logged (and thus
    /// undone) before the node itself.
    pub(crate) fn rollback(&mut self, cp: usize) {
        while self.undo_log.len() > cp {
            match self.undo_log.pop().unwrap() {
                FetchGraphOp::AddNode {
                    node_index,
                    root_key,
                } => {
                    self.bump_stage_count(self.depth[node_index.index()], -1);
                    self.graph.remove_node(node_index);
                    if let Some(key) = root_key {
                        self.root_groups.remove(&key);
                    }
                }
                FetchGraphOp::AddEdge(idx) => {
                    self.graph.remove_edge(idx);
                }
                FetchGraphOp::AppendEdgeInput(idx) => {
                    self.graph[idx].inputs.pop();
                }
                FetchGraphOp::ModifySelection {
                    node_index,
                    prev_head,
                } => {
                    self.graph[node_index]
                        .selection_builder
                        .restore_head(prev_head);
                }
                FetchGraphOp::RaiseDepth { node_index, prev } => {
                    self.bump_stage_count(self.depth[node_index.index()], -1);
                    self.bump_stage_count(prev, 1);
                    self.depth[node_index.index()] = prev;
                }
            }
        }
    }

    /// Add a `FetchNode`, registering its depth and logging for rollback.
    /// With `root_key` set, also registers it as a root group. Entity nodes
    /// claim the `entity_groups` reuse slot for their key if free.
    fn insert_node(&mut self, node: FetchNode, root_key: Option<Arc<str>>) -> NodeIndex {
        let entity_key = match &node.kind {
            FetchGroupKind::Entity { merge_at } => Some((node.subgraph.clone(), merge_at.clone())),
            _ => None,
        };
        let id = self.graph.add_node(node);
        if let Some(key) = &root_key {
            self.root_groups.insert(key.clone(), id);
        }
        self.register_node_depth(id);
        self.undo_log.push(FetchGraphOp::AddNode {
            node_index: id,
            root_key,
        });
        // Claim the entity_groups reuse slot if this is the first node for
        // this (subgraph, merge_at) pair. The undo log entry for AddNode
        // already handles removing the node; cleaning up the entity_groups
        // slot is handled inline in rollback by checking whether the slot
        // points to the removed node.
        if let Some(key) = entity_key {
            self.entity_groups.entry(key).or_insert(id);
        }
        id
    }

    /// Get or create the root fetch group for a subgraph.
    pub(crate) fn get_or_create_root_group(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
    ) -> NodeIndex {
        if let Some(&id) = self.root_groups.get(subgraph) {
            return id;
        }
        self.insert_node(
            FetchNode::new(subgraph.clone(), FetchGroupKind::Root { root_type }),
            Some(subgraph.clone()),
        )
    }

    /// Create a new entity fetch group.
    pub(crate) fn add_entity_group(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode::new(subgraph.clone(), FetchGroupKind::Entity { merge_at }),
            None,
        )
    }

    pub(crate) fn add_root_hop_group(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
        merge_at: Vec<FetchDataPathElement>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode::new(
                subgraph.clone(),
                FetchGroupKind::RootHop {
                    root_type,
                    merge_at,
                },
            ),
            None,
        )
    }

    /// Get or create the entity fetch group for (subgraph, merge_at).
    pub(crate) fn get_or_create_entity_group(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
    ) -> NodeIndex {
        let key = (subgraph.clone(), merge_at);
        if let Some(&id) = self.entity_groups.get(&key) {
            if self.graph.contains_node(id) {
                return id;
            }
            // Stale entry from a rolled-back node; remove and fall through.
            self.entity_groups.remove(&key);
        }
        self.add_entity_group(subgraph, key.1)
    }

    /// Whether a directed edge from `parent` to `child` exists.
    pub(crate) fn has_edge(&self, parent: NodeIndex, child: NodeIndex) -> bool {
        self.find_edge(parent, child).is_some()
    }

    /// Find the edge index for a directed edge from `parent` to `child`.
    pub(crate) fn find_edge(&self, parent: NodeIndex, child: NodeIndex) -> Option<EdgeIndex> {
        self.graph
            .edges_directed(parent, Direction::Outgoing)
            .find(|e| e.target() == child)
            .map(|e| e.id())
    }

    /// Create a parent->child dependency edge with the given inputs. The
    /// only way to create edges.
    pub(crate) fn add_dependency(
        &mut self,
        parent: NodeIndex,
        child: NodeIndex,
        inputs: Vec<InputContribution>,
    ) -> EdgeIndex {
        debug_assert_ne!(
            parent, child,
            "self-loop in FetchGraph: node {:?} ({}) cannot depend on itself",
            parent, self.graph[parent].subgraph,
        );
        let id = self
            .graph
            .add_edge(parent, child, FetchEdgeWeight { inputs });
        self.undo_log.push(FetchGraphOp::AddEdge(id));
        self.raise_depth(child, self.depth[parent.index()] + 1);
        id
    }

    /// Add an ordering-only dependency edge (no inputs) unless one exists.
    /// Errors if it would create a cycle: if the child already
    /// (transitively) feeds the parent, no valid execution order exists and
    /// the caller must fail this resolution branch.
    pub(crate) fn add_ordering_dependency(
        &mut self,
        parent: NodeIndex,
        child: NodeIndex,
    ) -> Result<(), FederationError> {
        if parent == child || self.has_edge(parent, child) {
            return Ok(());
        }
        if self.is_reachable(child, parent) {
            return Err(FederationError::internal(format!(
                "ordering dependency {:?} -> {:?} would create a cycle in the fetch graph",
                parent, child,
            )));
        }
        self.add_dependency(parent, child, vec![]);
        Ok(())
    }

    /// Whether the edge already carries a key input (`rewrite_info` set)
    /// for the given source type.
    pub(crate) fn edge_has_key_input(&self, edge: EdgeIndex, source_type: &Name) -> bool {
        self.graph[edge]
            .inputs
            .iter()
            .any(|i| i.rewrite_info.is_some() && i.source_type_name == *source_type)
    }

    /// Get a reference to an edge's weight.
    #[allow(dead_code)]
    pub(crate) fn edge_weight_raw(&self, edge: EdgeIndex) -> &FetchEdgeWeight {
        &self.graph[edge]
    }

    /// Append an input to an existing edge (e.g. @requires conditions
    /// added to a key-hop edge).
    pub(crate) fn add_input_to_edge(&mut self, edge: EdgeIndex, input: InputContribution) {
        self.undo_log.push(FetchGraphOp::AppendEdgeInput(edge));
        self.graph[edge].inputs.push(input);
    }

    /// Append a selection to a node's SelectionBuilder.
    pub(crate) fn append_selection(
        &mut self,
        node: NodeIndex,
        path: &SharedPath<Arc<OpPathElement>>,
        selections: Option<&Arc<SelectionSet>>,
    ) {
        let prev_head = self.graph[node].selection_builder.save_head();
        self.undo_log.push(FetchGraphOp::ModifySelection {
            node_index: node,
            prev_head,
        });
        self.graph[node].selection_builder.insert(path, selections);
    }

    /// Get a reference to the node weight.
    #[allow(dead_code)]
    pub(crate) fn node(&self, node: NodeIndex) -> &FetchNode {
        &self.graph[node]
    }

    /// Whether `node` refers to a live node (false for placeholders like
    /// `NodeIndex::end()` on uncommitted federated-root pendings).
    #[allow(dead_code)]
    pub(crate) fn contains_node(&self, node: NodeIndex) -> bool {
        self.graph.contains_node(node)
    }

    /// The merge_at path for a node; empty for root groups.
    pub(crate) fn merge_at(&self, node: NodeIndex) -> &[FetchDataPathElement] {
        match &self.graph[node].kind {
            FetchGroupKind::Root { .. } => &[],
            FetchGroupKind::Entity { merge_at } | FetchGroupKind::RootHop { merge_at, .. } => {
                merge_at
            }
        }
    }

    pub(crate) fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    #[allow(dead_code)]
    pub(crate) fn node_indices(&self) -> impl Iterator<Item = NodeIndex> + '_ {
        self.graph.node_indices()
    }

    pub(crate) fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Structural cost: each group contributes FETCH_COST scaled by a
    /// PIPELINING_COST multiplier for its pipeline depth.
    ///
    /// O(max_depth) via the incrementally maintained `stage_counts`. Only
    /// valid during search. Post-search mutations set `depth_dirty`; the
    /// final plan cost is computed independently by `plan_builder`.
    pub(crate) fn cost(&self) -> QueryPlanCost {
        debug_assert!(
            !self.depth_dirty,
            "cost() called after merge_sibling_entities invalidated stage counts"
        );
        self.stage_counts
            .iter()
            .enumerate()
            .map(|(i, &count)| count as f64 * FETCH_COST * (1.0f64).max(i as f64 * PIPELINING_COST))
            .sum()
    }

    /// Whether `to` is reachable from `from` via directed edges.
    pub(crate) fn is_reachable(&self, from: NodeIndex, to: NodeIndex) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![from];
        while let Some(node) = stack.pop() {
            if node == to {
                return true;
            }
            if visited.insert(node) {
                for edge in self.graph.edges_directed(node, Direction::Outgoing) {
                    stack.push(edge.target());
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use petgraph::Direction;
    use petgraph::stable_graph::NodeIndex;
    use petgraph::visit::EdgeRef;

    use super::*;

    fn dummy_root_type() -> CompositeTypeDefinitionPosition {
        CompositeTypeDefinitionPosition::Object(
            crate::schema::position::ObjectTypeDefinitionPosition {
                type_name: apollo_compiler::name!("Query"),
            },
        )
    }

    #[test]
    fn new_graph_is_empty() {
        let graph = FetchGraph::new();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
        assert!(graph.root_groups.is_empty());
    }

    #[test]
    fn get_or_create_root_group_is_idempotent() {
        let mut graph = FetchGraph::new();
        let subgraph: Arc<str> = Arc::from("subgraph_a");
        let root_type = dummy_root_type();

        let id1 = graph.get_or_create_root_group(&subgraph, root_type.clone());
        let id2 = graph.get_or_create_root_group(&subgraph, root_type);

        assert_eq!(id1, id2);
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn add_entity_group_creates_new_node() {
        let mut graph = FetchGraph::new();
        let subgraph: Arc<str> = Arc::from("subgraph_b");

        let node = graph.add_entity_group(&subgraph, vec![]);
        assert_eq!(graph.node_count(), 1);
        assert!(matches!(
            graph.graph[node].kind,
            FetchGroupKind::Entity { .. }
        ));
    }

    #[test]
    fn add_dependency_creates_edge_with_inputs() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        let entity = graph.add_entity_group(&sg, vec![]);

        let edge = graph.add_dependency(root, entity, vec![]);
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.graph[edge].inputs.is_empty());
    }

    #[test]
    fn add_input_to_edge_appends() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        let entity = graph.add_entity_group(&sg, vec![]);
        let edge = graph.add_dependency(root, entity, vec![]);

        // We can't easily construct a full InputContribution in a unit test
        // without a real schema, but we can verify the edge exists and
        // inputs is initially empty.
        assert_eq!(graph.graph[edge].inputs.len(), 0);
    }

    #[test]
    fn cost_empty_graph_is_zero() {
        let graph = FetchGraph::new();
        assert_eq!(graph.cost(), 0.0);
    }

    #[test]
    fn cost_single_root_is_fetch_cost() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        graph.get_or_create_root_group(&sg, dummy_root_type());
        // Single node at depth 0: FETCH_COST * max(1.0, 0 * PIPELINING_COST) = 1000.0
        assert_eq!(graph.cost(), 1000.0);
    }

    #[test]
    fn cost_accounts_for_depth() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        let child = graph.add_entity_group(&sg, vec![]);
        graph.add_dependency(root, child, vec![]);

        // Depth 0: 1 node x 1000.0 x max(1.0, 0 x 100.0) = 1000.0
        // Depth 1: 1 node x 1000.0 x max(1.0, 1 x 100.0) = 100000.0
        // Total = 101000.0
        assert_eq!(graph.cost(), 101000.0);
    }

    #[test]
    fn merge_at_root_is_empty() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        assert!(graph.merge_at(root).is_empty());
    }

    #[test]
    fn merge_at_entity_returns_path() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let path = vec![FetchDataPathElement::Key(
            apollo_compiler::name!("user"),
            Default::default(),
        )];
        let entity = graph.add_entity_group(&sg, path.clone());
        assert_eq!(graph.merge_at(entity).len(), 1);
    }

    #[test]
    fn clone_produces_independent_copy() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());

        let snapshot = graph.clone();

        // Mutate original; snapshot should be unaffected.
        graph.add_entity_group(&sg, vec![]);
        assert_eq!(graph.node_count(), 2);
        assert_eq!(snapshot.node_count(), 1);

        // Root group lookup still works on both.
        assert_eq!(graph.get_or_create_root_group(&sg, dummy_root_type()), root);
    }

    // --- Undo log tests ---

    #[test]
    fn checkpoint_rollback_add_entity_group() {
        let mut g = FetchGraph::new();
        let cp = g.checkpoint();
        let sg: Arc<str> = Arc::from("sg");
        g.add_entity_group(&sg, vec![]);
        assert_eq!(g.node_count(), 1);
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn checkpoint_rollback_add_root_group() {
        let mut g = FetchGraph::new();
        let cp = g.checkpoint();
        let sg: Arc<str> = Arc::from("sg");
        g.get_or_create_root_group(&sg, dummy_root_type());
        assert_eq!(g.node_count(), 1);
        assert!(g.root_groups.contains_key(&sg));
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
        assert!(!g.root_groups.contains_key(&sg));

        // Re-creating after rollback should work.
        g.get_or_create_root_group(&sg, dummy_root_type());
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn checkpoint_rollback_add_dependency() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let cp = g.checkpoint();
        let child = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, child, vec![]);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.node_count(), 2);
        g.rollback(cp);
        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.node_count(), 1); // only root remains
    }

    #[test]
    fn nested_checkpoints() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp1 = g.checkpoint();
        g.add_entity_group(&sg, vec![]);
        let cp2 = g.checkpoint();
        g.add_entity_group(&sg, vec![]);
        assert_eq!(g.node_count(), 2);
        g.rollback(cp2);
        assert_eq!(g.node_count(), 1);
        g.rollback(cp1);
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn rollback_then_forward() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp = g.checkpoint();
        g.add_entity_group(&sg, vec![]);
        g.rollback(cp);
        g.add_entity_group(&sg, vec![]);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn rollback_preserves_pre_checkpoint_state() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());

        let cp = g.checkpoint();
        let child = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, child, vec![]);
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);

        g.rollback(cp);
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
        // Root group still exists.
        assert_eq!(g.get_or_create_root_group(&sg, dummy_root_type()), root);
    }

    #[test]
    fn cost_after_rollback_matches_checkpoint_state() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let cost_before = g.cost();

        let cp = g.checkpoint();
        let child = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, child, vec![]);
        assert_ne!(g.cost(), cost_before);

        g.rollback(cp);
        assert_eq!(g.cost(), cost_before);
    }

    fn user_path(conditions: Option<Vec<Name>>) -> Vec<FetchDataPathElement> {
        vec![FetchDataPathElement::Key(
            apollo_compiler::name!("user"),
            conditions,
        )]
    }

    #[test]
    fn rollback_restores_depths_through_raise_depth_chain() {
        // Attaching an existing subtree under a deeper parent raises the whole
        // chain (raise_depth recursion); rollback must restore every stage count.
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let a = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, a, vec![]);

        let sg2: Arc<str> = Arc::from("sg2");
        let c = g.add_entity_group(&sg2, vec![]);
        let d = g.add_entity_group(&sg2, user_path(None));
        g.add_dependency(c, d, vec![]);

        // root(0), a(1), c(0), d(1): 2x1000 + 2x100000
        assert_eq!(g.cost(), 202_000.0);

        let cp = g.checkpoint();
        g.add_dependency(a, c, vec![]);
        // root(0), a(1), c(2), d(3): 1000 + 100000 + 200000 + 300000
        assert_eq!(g.cost(), 601_000.0);

        g.rollback(cp);
        assert_eq!(g.cost(), 202_000.0);
    }

    /// From-scratch cost oracle: recompute longest-path depths in topological
    /// order and apply the same FETCH_COST/PIPELINING_COST formula as `cost()`.
    /// Guards the incremental depth/stage-count bookkeeping against drift.
    fn recomputed_cost(g: &FetchGraph) -> QueryPlanCost {
        let order = petgraph::algo::toposort(&g.graph, None)
            .expect("fetch graph must be acyclic during search");
        let mut depth: HashMap<NodeIndex, u32> = HashMap::with_capacity(order.len());
        let mut total: QueryPlanCost = 0.0;
        for node in order {
            let d = g
                .graph
                .edges_directed(node, Direction::Incoming)
                .map(|e| depth[&e.source()] + 1)
                .max()
                .unwrap_or(0);
            depth.insert(node, d);
            total += FETCH_COST * (1.0f64).max(d as f64 * PIPELINING_COST);
        }
        total
    }

    #[test]
    fn incremental_cost_matches_recomputation_through_adds_raises_and_rollbacks() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let sg2: Arc<str> = Arc::from("sg2");
        assert_eq!(g.cost(), recomputed_cost(&g));

        // Adds: two independent chains.
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let a = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, a, vec![]);
        let b = g.add_entity_group(&sg2, vec![]);
        g.add_dependency(a, b, vec![]);
        let c = g.add_entity_group(&sg2, user_path(None));
        let d = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(c, d, vec![]);
        assert_eq!(g.cost(), recomputed_cost(&g));

        // Raise: attaching the (c, d) subtree under b propagates depth
        // increases through raise_depth's descendant recursion.
        let cp_outer = g.checkpoint();
        g.add_dependency(b, c, vec![]);
        assert_eq!(g.cost(), recomputed_cost(&g));

        // A second parent edge that does NOT raise (a is shallower than b).
        g.add_dependency(a, c, vec![]);
        assert_eq!(g.cost(), recomputed_cost(&g));

        // Nested checkpoint: extend the deep chain, then roll it back.
        let cp_inner = g.checkpoint();
        let e = g.add_entity_group(&sg, vec![]);
        g.add_dependency(d, e, vec![]);
        assert_eq!(g.cost(), recomputed_cost(&g));
        g.rollback(cp_inner);
        assert_eq!(g.cost(), recomputed_cost(&g));

        // Outer rollback undoes the raises too.
        g.rollback(cp_outer);
        assert_eq!(g.cost(), recomputed_cost(&g));

        // Build forward again after rollback.
        g.add_dependency(b, c, vec![]);
        assert_eq!(g.cost(), recomputed_cost(&g));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "self-loop")]
    fn add_dependency_self_loop_panics() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let node = g.add_entity_group(&sg, vec![]);
        g.add_dependency(node, node, vec![]);
    }

    // --- Ordering dependencies ---

    #[test]
    fn add_ordering_dependency_rejects_cycle() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let mid = g.add_entity_group(&sg, vec![]);
        let leaf = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(root, mid, vec![]);
        g.add_dependency(mid, leaf, vec![]);

        // leaf transitively feeds from root, so root must not depend on leaf.
        let result = g.add_ordering_dependency(leaf, root);
        assert!(result.is_err());
        assert_eq!(g.edge_count(), 2);
    }

    #[test]
    fn add_ordering_dependency_noop_cases() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let child = g.add_entity_group(&sg, vec![]);
        g.add_dependency(root, child, vec![]);

        // Self-edge: no-op.
        assert!(g.add_ordering_dependency(root, root).is_ok());
        // Existing edge: no-op.
        assert!(g.add_ordering_dependency(root, child).is_ok());
        assert_eq!(g.edge_count(), 1);

        // New acyclic ordering edge is created.
        let other = g.add_entity_group(&sg, user_path(None));
        assert!(g.add_ordering_dependency(child, other).is_ok());
        assert_eq!(g.edge_count(), 2);
        assert!(g.has_edge(child, other));
    }
}
