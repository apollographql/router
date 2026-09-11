//! The fetch graph: fetch groups (nodes), dependencies (edges), and the
//! entity inputs riding those edges, built incrementally during BULB
//! search with checkpoint / undo-log rollback.

#[allow(dead_code)]
pub(crate) mod plan_builder;
pub(crate) mod selection_builder;

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::Direction;
use petgraph::stable_graph::EdgeIndex;
use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::EdgeRef;
use petgraph::visit::NodeIndexable;
use selection_builder::SelectionBuilder;
use selection_builder::SelectionCheckpoint;

use super::shared_path::SharedPath;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::QueryPlanCost;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;

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
        /// The root operation kind this hop resolves through. Carried per
        /// hop rather than taken from the surrounding operation: a hop
        /// through a subgraph's query root inside a mutation plan must
        /// still build (and label) a query operation.
        root_kind: SchemaRootDefinitionKind,
        merge_at: Vec<FetchDataPathElement>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum InputContribution {
    /// @key fields the parent sends to enter the child; drives input
    /// rewrites on the key fetch.
    Key {
        source_type_name: Name,
        conditions: Arc<SelectionSet>,
        rewrite_info: InputRewriteInfo,
    },
    /// @requires condition fields riding an existing edge. Constructed by
    /// the requires support in a later change.
    #[allow(dead_code)]
    Requires {
        source_type_name: Name,
        conditions: Arc<SelectionSet>,
    },
}

impl InputContribution {
    pub(crate) fn source_type_name(&self) -> &Name {
        match self {
            Self::Key {
                source_type_name, ..
            }
            | Self::Requires {
                source_type_name, ..
            } => source_type_name,
        }
    }

    pub(crate) fn conditions(&self) -> &Arc<SelectionSet> {
        match self {
            Self::Key { conditions, .. } | Self::Requires { conditions, .. } => conditions,
        }
    }

    pub(crate) fn rewrite_info(&self) -> Option<&InputRewriteInfo> {
        match self {
            Self::Key { rewrite_info, .. } => Some(rewrite_info),
            Self::Requires { .. } => None,
        }
    }
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
    /// A node was added. Undo: remove it and release its root_groups /
    /// entity_groups entries (entity slots only when this node owns them;
    /// node indices are recycled, so stale entries must never linger).
    AddNode {
        node_index: NodeIndex,
        root_key: Option<Arc<str>>,
        entity_key: Option<EntityGroupKey>,
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
}

/// Index key for entity fetch group reuse.
type EntityGroupKey = (Arc<str>, Vec<FetchDataPathElement>);

/// Opaque undo checkpoint: the undo log length at a point in time.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraphCheckpoint(usize);

/// Lightweight fetch graph for BULB search. Trial branches are applied,
/// scored, and undone on one instance via an append-only mutation log:
/// `checkpoint()` marks a log position, `rollback(cp)` reverses back to it.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraph {
    graph: StableDiGraph<FetchNode, FetchEdgeWeight>,
    /// Root groups keyed by subgraph name. One search plans one root kind,
    /// so the kind is not part of the key; root hops never register here.
    root_groups: HashMap<Arc<str>, NodeIndex>,
    /// First-created entity group per (subgraph, merge_at), so group reuse
    /// is a lookup instead of a node scan. The first node with a key owns
    /// the slot; LIFO undo releases it with that node.
    entity_groups: HashMap<EntityGroupKey, NodeIndex>,
    undo_log: Vec<FetchGraphOp>,
}

impl FetchGraph {
    pub(crate) fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            root_groups: HashMap::new(),
            entity_groups: HashMap::new(),
            undo_log: Vec::new(),
        }
    }

    /// Current undo log position, for `rollback()`.
    pub(crate) fn checkpoint(&self) -> FetchGraphCheckpoint {
        FetchGraphCheckpoint(self.undo_log.len())
    }

    /// Undo all mutations back to the checkpoint, replaying log entries in
    /// reverse.
    pub(crate) fn rollback(&mut self, cp: FetchGraphCheckpoint) {
        debug_assert!(
            cp.0 <= self.undo_log.len(),
            "checkpoint is newer than the log; checkpoints must be restored in LIFO order",
        );
        while self.undo_log.len() > cp.0 {
            match self.undo_log.pop().unwrap() {
                FetchGraphOp::AddNode {
                    node_index,
                    root_key,
                    entity_key,
                } => {
                    self.graph.remove_node(node_index);
                    if let Some(key) = root_key {
                        self.root_groups.remove(&key);
                    }
                    // A later duplicate node never claimed the slot.
                    if let Some(key) = entity_key
                        && self.entity_groups.get(&key) == Some(&node_index)
                    {
                        self.entity_groups.remove(&key);
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
            }
        }
    }

    /// Add a `FetchNode`, logging for rollback. `root_key` registers a
    /// root group; entity nodes claim their `entity_groups` slot if free.
    fn insert_node(&mut self, node: FetchNode, root_key: Option<Arc<str>>) -> NodeIndex {
        let entity_key = match &node.kind {
            FetchGroupKind::Entity { merge_at } => Some((node.subgraph.clone(), merge_at.clone())),
            _ => None,
        };
        let id = self.graph.add_node(node);
        if let Some(key) = &root_key {
            self.root_groups.insert(key.clone(), id);
        }
        if let Some(key) = &entity_key {
            self.entity_groups.entry(key.clone()).or_insert(id);
        }
        self.undo_log.push(FetchGraphOp::AddNode {
            node_index: id,
            root_key,
            entity_key,
        });
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
        root_kind: SchemaRootDefinitionKind,
        merge_at: Vec<FetchDataPathElement>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode::new(
                subgraph.clone(),
                FetchGroupKind::RootHop {
                    root_type,
                    root_kind,
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
            debug_assert!(
                self.graph.contains_node(id),
                "entity_groups slot points at a removed node; rollback cleanup is broken",
            );
            return id;
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
        // Everything downstream assumes acyclicity; is_reachable(x, x) is
        // true, so this also rejects self-loops.
        debug_assert!(
            !self.is_reachable(child, parent),
            "edge {:?} -> {:?} ({}) would create a cycle in the fetch graph",
            parent,
            child,
            self.graph[parent].subgraph,
        );
        let id = self
            .graph
            .add_edge(parent, child, FetchEdgeWeight { inputs });
        self.undo_log.push(FetchGraphOp::AddEdge(id));
        id
    }

    /// Add an ordering-only dependency edge (no inputs) unless one exists.
    /// Errors if it would create a cycle; the caller must fail the branch.
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

    /// Whether the edge already carries a key input for the given source
    /// type.
    pub(crate) fn edge_has_key_input(&self, edge: EdgeIndex, source_type: &Name) -> bool {
        self.graph[edge].inputs.iter().any(|i| {
            matches!(i, InputContribution::Key { source_type_name, .. }
                if source_type_name == source_type)
        })
    }

    /// Get a reference to an edge's weight.
    pub(crate) fn edge_weight(&self, edge: EdgeIndex) -> &FetchEdgeWeight {
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
    pub(crate) fn node(&self, node: NodeIndex) -> &FetchNode {
        &self.graph[node]
    }

    /// Clone for saving a completed candidate; drops the undo log, which
    /// snapshots never roll back.
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            undo_log: Vec::new(),
            graph: self.graph.clone(),
            root_groups: self.root_groups.clone(),
            entity_groups: self.entity_groups.clone(),
        }
    }

    /// Whether `node` refers to a live node (false for placeholder
    /// `NodeIndex` values a caller has not committed yet).
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

    pub(crate) fn node_indices(&self) -> impl Iterator<Item = NodeIndex> + '_ {
        self.graph.node_indices()
    }

    pub(crate) fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Structural cost: FETCH_COST per group, scaled by pipeline depth
    /// (longest parent chain). Recomputed per call; incremental caching is
    /// deferred until the search is proven correct.
    pub(crate) fn cost(&self) -> QueryPlanCost {
        let Ok(order) = petgraph::algo::toposort(&self.graph, None) else {
            debug_assert!(false, "cycle in fetch graph");
            return f64::MAX;
        };
        let mut depth = vec![0u32; self.graph.node_bound()];
        let mut total: QueryPlanCost = 0.0;
        for node in order {
            let d = self
                .graph
                .edges_directed(node, Direction::Incoming)
                .map(|e| depth[e.source().index()] + 1)
                .max()
                .unwrap_or(0);
            depth[node.index()] = d;
            total += FETCH_COST * (1.0f64).max(d as f64 * PIPELINING_COST);
        }
        total
    }

    /// Whether `to` is reachable from `from` via directed edges.
    pub(crate) fn is_reachable(&self, from: NodeIndex, to: NodeIndex) -> bool {
        petgraph::algo::has_path_connecting(&self.graph, from, to, None)
    }
}

#[cfg(test)]
mod tests {
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
    fn add_root_hop_group_round_trips() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let node = graph.add_root_hop_group(
            &sg,
            dummy_root_type(),
            SchemaRootDefinitionKind::Query,
            user_path(None),
        );
        let FetchGroupKind::RootHop {
            root_kind,
            merge_at,
            ..
        } = &graph.node(node).kind
        else {
            panic!("expected a root hop group");
        };
        assert_eq!(*root_kind, SchemaRootDefinitionKind::Query);
        assert_eq!(merge_at, &user_path(None));
        assert_eq!(graph.merge_at(node), user_path(None));
        assert_eq!(graph.node(node).root_type(), Some(&dummy_root_type()),);
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

    /// Build a real InputContribution from a tiny schema so the append and
    /// undo paths are exercised with representative data.
    fn test_input_contribution() -> InputContribution {
        let schema = apollo_compiler::schema::Schema::parse_and_validate(
            r#"
            type Query { user: User }
            type User { id: ID }
            "#,
            "schema.graphql",
        )
        .expect("valid schema");
        let schema =
            crate::schema::ValidFederationSchema::new(schema).expect("valid federation schema");
        let op = crate::operation::Operation::parse(schema, r#"{ user { id } }"#, "op.graphql")
            .expect("valid operation");
        InputContribution::Requires {
            source_type_name: apollo_compiler::name!("User"),
            conditions: Arc::new(op.selection_set.clone()),
        }
    }

    #[test]
    fn add_input_to_edge_appends() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        let entity = graph.add_entity_group(&sg, vec![]);
        let edge = graph.add_dependency(root, entity, vec![]);
        assert_eq!(graph.graph[edge].inputs.len(), 0);

        graph.add_input_to_edge(edge, test_input_contribution());
        assert_eq!(graph.graph[edge].inputs.len(), 1);
        assert_eq!(
            *graph.graph[edge].inputs[0].source_type_name(),
            apollo_compiler::name!("User"),
        );
    }

    #[test]
    fn rollback_removes_appended_edge_input() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = graph.get_or_create_root_group(&sg, dummy_root_type());
        let entity = graph.add_entity_group(&sg, vec![]);
        let edge = graph.add_dependency(root, entity, vec![]);
        graph.add_input_to_edge(edge, test_input_contribution());

        let cp = graph.checkpoint();
        graph.add_input_to_edge(edge, test_input_contribution());
        assert_eq!(graph.graph[edge].inputs.len(), 2);

        graph.rollback(cp);
        assert_eq!(graph.graph[edge].inputs.len(), 1);
    }

    #[test]
    fn rollback_restores_selection_builder_head() {
        let mut graph = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let node = graph.add_entity_group(&sg, vec![]);
        let path = SharedPath::new();

        graph.append_selection(node, &path, None);
        assert_eq!(graph.node(node).selection_builder.entries().len(), 1);

        let cp = graph.checkpoint();
        graph.append_selection(node, &path, None);
        graph.append_selection(node, &path, None);
        assert_eq!(graph.node(node).selection_builder.entries().len(), 3);

        graph.rollback(cp);
        assert_eq!(graph.node(node).selection_builder.entries().len(), 1);
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
    fn cost_tracks_depth_changes_through_rollback() {
        // Attaching an existing subtree under a deeper parent deepens the
        // whole chain; rollback must restore the original cost.
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

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "cycle")]
    fn add_dependency_self_loop_panics() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let node = g.add_entity_group(&sg, vec![]);
        g.add_dependency(node, node, vec![]);
    }

    /// A 2-cycle must be rejected up front: raise_depth assumes acyclicity
    /// and recurses forever (stack overflow, in release too) if an edge
    /// closing a cycle is ever inserted.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "cycle")]
    fn add_dependency_two_cycle_panics_instead_of_recursing_forever() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let a = g.add_entity_group(&sg, vec![]);
        let b = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(a, b, vec![]);
        g.add_dependency(b, a, vec![]);
    }

    /// A rolled-back entity group must not leave its reuse slot pointing at
    /// a recycled node index: StableDiGraph reuses removed indices, so a
    /// liveness check on the stale index can resolve to an unrelated node.
    #[test]
    fn rollback_does_not_leak_entity_group_slot_to_reused_index() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp = g.checkpoint();
        let original = g.get_or_create_entity_group(&sg, user_path(None));
        g.rollback(cp);

        // Reuse the freed index for an unrelated group (different merge_at).
        let unrelated = g.add_entity_group(&sg, vec![]);
        assert_eq!(
            unrelated.index(),
            original.index(),
            "test setup requires StableDiGraph to reuse the freed index",
        );

        let looked_up = g.get_or_create_entity_group(&sg, user_path(None));
        assert_ne!(
            looked_up, unrelated,
            "stale entity_groups slot resolved to an unrelated node",
        );
        let FetchGroupKind::Entity { merge_at } = &g.node(looked_up).kind else {
            panic!("expected an entity group");
        };
        assert_eq!(merge_at, &user_path(None));
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
