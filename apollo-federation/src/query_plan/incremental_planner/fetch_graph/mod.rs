//! The fetch graph: fetch groups (nodes), dependencies (edges), and the
//! entity inputs riding those edges, built incrementally during BULB
//! search with O(1) checkpoint / undo-log rollback.

#[allow(dead_code)]
pub(crate) mod plan_builder;
pub(crate) mod selection_builder;

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use indexmap::IndexMap;
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
use crate::query_plan::FetchDataKeyRenamer;
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
    /// Alias to original-name pairs for @requires condition fields aliased
    /// to avoid cross-fetch response path collisions; each generates an
    /// input KeyRenamer rewrite undoing the alias before the subgraph send.
    pub(crate) condition_alias_rewrites: Vec<(Name, Name)>,
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
    /// The @defer label this fetch belongs to; `None` for the primary
    /// (non-deferred) response. Fetch nodes with a defer_ref are partitioned
    /// into deferred blocks during plan generation.
    pub(crate) defer_ref: Option<String>,
    /// @fromContext rewrite paths that rename entity data keys to
    /// `$contextualArgument_N_M` variable names.
    pub(crate) context_rewrites: Vec<FetchDataKeyRenamer>,
    /// @fromContext variable definitions added to the subgraph operation.
    pub(crate) context_variables: Vec<(Name, Node<apollo_compiler::ast::Type>)>,
    /// When set, this fetch is backed by a connector rather than a GraphQL
    /// subgraph endpoint. Plan builder maps this to `FetchProtocol::Connector`.
    pub(crate) connector: Option<Arc<crate::connectors::Connector>>,
}

impl FetchNode {
    pub(crate) fn new(subgraph: Arc<str>, kind: FetchGroupKind) -> Self {
        Self {
            subgraph,
            kind,
            selection_builder: SelectionBuilder::default(),
            defer_ref: None,
            context_rewrites: Vec::new(),
            context_variables: Vec::new(),
            connector: None,
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
    /// A node was added. Undo: remove_node (StableDiGraph keeps indices
    /// stable); a root node (`root_key` set) also removes its root_groups
    /// entry.
    AddNode {
        node_index: NodeIndex,
        root_key: Option<(Arc<str>, Option<String>)>,
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
    /// @fromContext rewrites/variables were appended to a node. Undo:
    /// truncate both vecs to their prior lengths.
    AddContext {
        node_index: NodeIndex,
        prev_rewrites: usize,
        prev_variables: usize,
    },
    /// A node claimed the entity_groups slot for its key. Undo: remove the
    /// entry. Always logged directly after the node's AddNode, so LIFO
    /// undo clears the slot before removing the node.
    RegisterEntityGroup { key: EntityGroupKey },
}

/// Index key for entity fetch group reuse: (subgraph, merge_at, defer_ref).
type EntityGroupKey = (Arc<str>, Vec<FetchDataPathElement>, Option<String>);

/// Lightweight fetch graph for BULB search.
///
/// Undo works via an append-only mutation log: `checkpoint()` returns the
/// current log position, `rollback(cp)` reverses back to it. Trial
/// branches are applied, scored, and undone on a single instance, so no
/// cloning during search.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraph {
    graph: StableDiGraph<FetchNode, FetchEdgeWeight>,
    /// Root groups keyed by (subgraph, defer_ref): a deferred root fetch is
    /// a separate group from the primary root in the same subgraph.
    root_groups: HashMap<(Arc<str>, Option<String>), NodeIndex>,
    /// First-created entity group per (subgraph, merge_at, defer_ref), so
    /// `get_or_create_entity_group_with_defer` (run on every key hop) is a
    /// lookup instead of a node scan. Only the first node with a key claims
    /// the slot (earliest-index-wins); undo is LIFO, so a later duplicate
    /// can never outlive the slot owner. Stale after post-search sibling
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
                FetchGraphOp::AddContext {
                    node_index,
                    prev_rewrites,
                    prev_variables,
                } => {
                    let fetch_node = &mut self.graph[node_index];
                    fetch_node.context_rewrites.truncate(prev_rewrites);
                    fetch_node.context_variables.truncate(prev_variables);
                }
                FetchGraphOp::RegisterEntityGroup { key } => {
                    self.entity_groups.remove(&key);
                }
            }
        }
    }

    /// Add a `FetchNode`, registering its depth and logging for rollback.
    /// With `root_key` set, also registers it as a root group. Entity nodes
    /// claim the `entity_groups` reuse slot for their key if free.
    fn insert_node(
        &mut self,
        node: FetchNode,
        root_key: Option<(Arc<str>, Option<String>)>,
    ) -> NodeIndex {
        let entity_key = match &node.kind {
            FetchGroupKind::Entity { merge_at } => Some((
                node.subgraph.clone(),
                merge_at.clone(),
                node.defer_ref.clone(),
            )),
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
        if let Some(key) = entity_key
            && !self.entity_groups.contains_key(&key)
        {
            self.entity_groups.insert(key.clone(), id);
            self.undo_log
                .push(FetchGraphOp::RegisterEntityGroup { key });
        }
        id
    }

    /// Get or create the root fetch group for a subgraph (no defer scope).
    pub(crate) fn get_or_create_root_group(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
    ) -> NodeIndex {
        self.get_or_create_root_group_with_defer(subgraph, root_type, None)
    }

    /// Get or create the root fetch group for a (subgraph, defer_ref) pair.
    pub(crate) fn get_or_create_root_group_with_defer(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let root_key = (subgraph.clone(), defer_ref.clone());
        if let Some(&id) = self.root_groups.get(&root_key) {
            return id;
        }
        self.insert_node(
            FetchNode {
                subgraph: subgraph.clone(),
                kind: FetchGroupKind::Root { root_type },
                selection_builder: SelectionBuilder::default(),
                defer_ref,
                context_rewrites: Vec::new(),
                context_variables: Vec::new(),
                connector: None,
            },
            Some(root_key),
        )
    }

    /// Create a new entity fetch group with an explicit defer scope.
    pub(crate) fn add_entity_group_with_defer(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode {
                subgraph: subgraph.clone(),
                kind: FetchGroupKind::Entity { merge_at },
                selection_builder: SelectionBuilder::default(),
                defer_ref,
                context_rewrites: Vec::new(),
                context_variables: Vec::new(),
                connector: None,
            },
            None,
        )
    }

    /// Create a new entity fetch group (no defer scope).
    pub(crate) fn add_entity_group(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
    ) -> NodeIndex {
        self.add_entity_group_with_defer(subgraph, merge_at, None)
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

    /// Create a root fetch group backed by a connector.
    pub(crate) fn add_connector_root_group(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
        connector: Arc<crate::connectors::Connector>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode {
                subgraph: subgraph.clone(),
                kind: FetchGroupKind::Root { root_type },
                selection_builder: SelectionBuilder::default(),
                defer_ref,
                context_rewrites: Vec::new(),
                context_variables: Vec::new(),
                connector: Some(connector),
            },
            None,
        )
    }

    /// Create an entity fetch group backed by a connector. Never reused —
    /// each connector entity resolution is its own node.
    pub(crate) fn add_connector_entity_group(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        connector: Arc<crate::connectors::Connector>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode {
                subgraph: subgraph.clone(),
                kind: FetchGroupKind::Entity { merge_at },
                selection_builder: SelectionBuilder::default(),
                defer_ref,
                context_rewrites: Vec::new(),
                context_variables: Vec::new(),
                connector: Some(connector),
            },
            None,
        )
    }

    /// Get or create the entity fetch group for (subgraph, merge_at, defer_ref).
    pub(crate) fn get_or_create_entity_group_with_defer(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let key = (subgraph.clone(), merge_at, defer_ref);
        if let Some(&id) = self.entity_groups.get(&key) {
            if self.graph.contains_node(id) {
                return id;
            }
            self.entity_groups.remove(&key);
        }
        self.add_entity_group_with_defer(subgraph, key.1, key.2)
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
    pub(crate) fn edge_weight_raw(&self, edge: EdgeIndex) -> &FetchEdgeWeight {
        &self.graph[edge]
    }

    /// All input contributions on edges into `node`.
    pub(crate) fn incoming_inputs(
        &self,
        node: NodeIndex,
    ) -> impl Iterator<Item = &InputContribution> {
        self.graph
            .edges_directed(node, Direction::Incoming)
            .flat_map(|edge| edge.weight().inputs.iter())
    }

    /// Whether adding `new_rewrites` to `edge` would rename two different
    /// aliases to the same original field name.
    pub(crate) fn has_conflicting_condition_rewrites(
        &self,
        edge: EdgeIndex,
        new_rewrites: &[(Name, Name)],
    ) -> bool {
        let existing = &self.graph[edge].inputs;
        for (_alias, original) in new_rewrites {
            for input in existing {
                if input
                    .condition_alias_rewrites
                    .iter()
                    .any(|(_, orig)| orig == original)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Clone an edge's key-hop inputs (those with `rewrite_info`), for
    /// splitting an entity group.
    pub(crate) fn clone_key_inputs(&self, edge: EdgeIndex) -> Vec<InputContribution> {
        self.graph[edge]
            .inputs
            .iter()
            .filter(|i| i.rewrite_info.is_some())
            .cloned()
            .collect()
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

    /// Append a @fromContext rewrite and variable definition to a node,
    /// deduplicating by renamer identity / variable name.
    pub(crate) fn add_context(
        &mut self,
        node: NodeIndex,
        renamer: FetchDataKeyRenamer,
        context_id: Name,
        context_type: Node<apollo_compiler::ast::Type>,
    ) {
        let fetch_node = &mut self.graph[node];
        let prev_rewrites = fetch_node.context_rewrites.len();
        let prev_variables = fetch_node.context_variables.len();
        if !fetch_node.context_rewrites.contains(&renamer) {
            fetch_node.context_rewrites.push(renamer);
        }
        if !fetch_node
            .context_variables
            .iter()
            .any(|(n, _)| *n == context_id)
        {
            fetch_node
                .context_variables
                .push((context_id, context_type));
        }
        if fetch_node.context_rewrites.len() != prev_rewrites
            || fetch_node.context_variables.len() != prev_variables
        {
            self.undo_log.push(FetchGraphOp::AddContext {
                node_index: node,
                prev_rewrites,
                prev_variables,
            });
        }
    }

    /// Get a reference to the node weight.
    pub(crate) fn node(&self, node: NodeIndex) -> &FetchNode {
        &self.graph[node]
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

    /// Merge entity nodes sharing the same (subgraph, merge_at) into one
    /// node. Called once post-search on the winning candidate.
    ///
    /// Grouping ignores type conditions on merge_at elements: siblings
    /// differing only in concrete type are merged, the widened path merely
    /// offering extra candidate objects that the `requires` representations
    /// (still gated by `__typename`) reject. Without this, deeply nested
    /// polymorphic queries fragment into one fetch per concrete-type
    /// combination.
    ///
    /// Transitively dependent nodes are NOT merged — that would create
    /// cycles (multi-hop @requires chains can revisit a subgraph at
    /// different stages).
    #[allow(clippy::type_complexity)]
    pub(crate) fn merge_sibling_entities(&mut self) {
        // Group by (subgraph, condition-stripped merge_at).
        // IndexMap for deterministic processing order: when merged groups
        // have edges to each other, order decides how relocated edge inputs
        // interleave, which is visible in the serialized plan.
        let mut groups: IndexMap<(Arc<str>, Vec<FetchDataPathElement>), Vec<NodeIndex>> =
            IndexMap::new();
        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];
            if let FetchGroupKind::Entity { merge_at } = &node.kind {
                let key = (node.subgraph.clone(), strip_merge_at_conditions(merge_at));
                groups.entry(key).or_default().push(node_idx);
            }
        }

        for (_key, group) in groups {
            if group.len() <= 1 {
                continue;
            }

            // Partition into sets with no transitive dependency between members.
            let merge_sets = self.partition_by_reachability(&group);

            for set in merge_sets {
                if set.len() <= 1 {
                    continue;
                }
                for bucket in self.bucket_by_merge_compatibility(set) {
                    if bucket.len() <= 1 {
                        continue;
                    }
                    let survivor = bucket[0];
                    self.union_merge_at_conditions(&bucket);
                    self.merge_nodes_into(survivor, &bucket[1..]);
                }
            }
        }
    }

    /// Bucket a merge set into merge-compatible subsets. Nodes are
    /// incompatible when their selections assign different field signatures
    /// to the same response path (e.g. `value` vs `value(scale: 100)`) or
    /// when their input conditions disagree for the same source type — the
    /// merged entity representation cannot satisfy both branches.
    fn bucket_by_merge_compatibility(&self, mergeable: Vec<NodeIndex>) -> Vec<Vec<NodeIndex>> {
        struct Bucket {
            signatures: HashMap<String, String>,
            merge_at: Option<Vec<FetchDataPathElement>>,
            input_conditions: HashMap<Name, BTreeSet<String>>,
            nodes: Vec<NodeIndex>,
        }
        let mut buckets: Vec<Bucket> = Vec::new();
        for n in mergeable {
            let signatures = self.graph[n].selection_builder.field_signatures();
            let input_conditions = self.input_condition_fingerprints(n);
            let FetchGroupKind::Entity { merge_at } = &self.graph[n].kind else {
                continue;
            };
            let merge_at = merge_at.clone();
            match buckets.iter_mut().find(|bucket| {
                signatures.iter().all(|(path, signature)| {
                    bucket
                        .signatures
                        .get(path)
                        .is_none_or(|taken| taken == signature)
                }) && input_conditions.iter().all(|(ty, conditions)| {
                    bucket
                        .input_conditions
                        .get(ty)
                        .is_none_or(|taken| taken == conditions)
                })
            }) {
                Some(bucket) => {
                    bucket.signatures.extend(signatures);
                    if bucket.merge_at.as_ref() != Some(&merge_at) {
                        bucket.merge_at = None;
                    }
                    for (ty, conditions) in input_conditions {
                        bucket
                            .input_conditions
                            .entry(ty)
                            .or_default()
                            .extend(conditions);
                    }
                    bucket.nodes.push(n);
                }
                None => buckets.push(Bucket {
                    signatures,
                    merge_at: Some(merge_at),
                    input_conditions,
                    nodes: vec![n],
                }),
            }
        }
        buckets.into_iter().map(|bucket| bucket.nodes).collect()
    }

    /// Condition selections this node's entity representation receives per
    /// source type, rendered to strings for cheap set comparison. Two nodes
    /// disagreeing here would union into a per-type representation neither
    /// branch's runtime objects satisfy.
    fn input_condition_fingerprints(&self, node: NodeIndex) -> HashMap<Name, BTreeSet<String>> {
        let mut fingerprints: HashMap<Name, BTreeSet<String>> = HashMap::new();
        for edge in self.graph.edges_directed(node, Direction::Incoming) {
            for input in &edge.weight().inputs {
                fingerprints
                    .entry(input.source_type_name.clone())
                    .or_default()
                    .insert(input.conditions.to_string());
            }
        }
        fingerprints
    }

    /// Rewrite the bucket's merge_at paths to the shared condition-stripped
    /// path when members' type conditions differ. The widened flatten path
    /// offers extra candidate objects at runtime, but entity
    /// representations still gate on `__typename`, so non-matching objects
    /// contribute nothing.
    fn union_merge_at_conditions(&mut self, bucket: &[NodeIndex]) {
        let Some((&first, rest)) = bucket.split_first() else {
            return;
        };
        let FetchGroupKind::Entity { merge_at } = &self.graph[first].kind else {
            return;
        };
        if rest.iter().all(|&n| {
            matches!(&self.graph[n].kind, FetchGroupKind::Entity { merge_at: other } if other == merge_at)
        }) {
            return;
        }
        let stripped = strip_merge_at_conditions(merge_at);
        for &n in bucket {
            if let FetchGroupKind::Entity { merge_at } = &mut self.graph[n].kind {
                *merge_at = stripped.clone();
            }
        }
    }

    /// Partition a group into sets where no member is transitively
    /// reachable from another member of the same set.
    ///
    /// The common case (type-explosion siblings, no inter-dependencies) is
    /// handled by a cheap direct-edge check; per-member BFS is the fallback.
    fn partition_by_reachability(&self, group: &[NodeIndex]) -> Vec<Vec<NodeIndex>> {
        let member_set: HashSet<NodeIndex> = group.iter().copied().collect();

        // Fast path: no direct edges between members, O(G * avg_out_degree).
        let has_direct_edge = group.iter().any(|&node| {
            self.graph
                .edges_directed(node, Direction::Outgoing)
                .any(|e| member_set.contains(&e.target()))
        });
        if !has_direct_edge {
            // Transitive paths through non-members remain possible, but
            // need a shared intermediate — impossible when every member is
            // a leaf (out-degree 0), which makes them trivially independent.
            let all_leaves = group.iter().all(|&node| {
                self.graph
                    .edges_directed(node, Direction::Outgoing)
                    .next()
                    .is_none()
            });
            if all_leaves {
                return vec![group.to_vec()];
            }
        }

        // General case: BFS from each member to find reachable group peers.
        let member_index: HashMap<NodeIndex, usize> =
            group.iter().enumerate().map(|(i, &n)| (n, i)).collect();

        let mut reachable_from: Vec<HashSet<usize>> = Vec::with_capacity(group.len());
        for (src_idx, &node) in group.iter().enumerate() {
            let mut reached = HashSet::new();
            let mut visited = HashSet::new();
            let mut stack = vec![node];
            while let Some(current) = stack.pop() {
                if !visited.insert(current) {
                    continue;
                }
                if let Some(&idx) = member_index.get(&current)
                    && idx != src_idx
                {
                    reached.insert(idx);
                }
                for edge in self.graph.edges_directed(current, Direction::Outgoing) {
                    stack.push(edge.target());
                }
            }
            reachable_from.push(reached);
        }

        let mut sets: Vec<Vec<usize>> = Vec::new();
        'outer: for i in 0..group.len() {
            for set in &mut sets {
                let conflict = set
                    .iter()
                    .any(|&j| reachable_from[i].contains(&j) || reachable_from[j].contains(&i));
                if !conflict {
                    set.push(i);
                    continue 'outer;
                }
            }
            sets.push(vec![i]);
        }

        sets.into_iter()
            .map(|set| set.into_iter().map(|i| group[i]).collect())
            .collect()
    }

    /// Merge nodes into a survivor, relocating edges and absorbing selections.
    fn merge_nodes_into(&mut self, survivor: NodeIndex, to_merge: &[NodeIndex]) {
        // Removals and relocations below bypass the incremental depth
        // bookkeeping. Merging runs once, post-search, so nothing rolls
        // back past this.
        self.depth_dirty = true;
        for &merged in to_merge {
            // Absorb selections from the merged node.
            let merged_builder = self.graph[merged].selection_builder.clone();
            self.graph[survivor]
                .selection_builder
                .merge_from(&merged_builder);

            // Relocate incoming edges.
            let incoming: Vec<_> = self
                .graph
                .edges_directed(merged, Direction::Incoming)
                .map(|e| (e.source(), e.weight().inputs.clone()))
                .collect();
            for (parent, inputs) in incoming {
                if parent == survivor {
                    continue;
                }
                if let Some(existing) = self.find_edge(parent, survivor) {
                    self.graph[existing].inputs.extend(inputs);
                } else {
                    self.graph
                        .add_edge(parent, survivor, FetchEdgeWeight { inputs });
                }
            }

            // Relocate outgoing edges.
            let outgoing: Vec<_> = self
                .graph
                .edges_directed(merged, Direction::Outgoing)
                .map(|e| (e.target(), e.weight().inputs.clone()))
                .collect();
            for (child, inputs) in outgoing {
                if child == survivor {
                    continue;
                }
                if let Some(existing) = self.find_edge(survivor, child) {
                    self.graph[existing].inputs.extend(inputs);
                } else {
                    self.graph
                        .add_edge(survivor, child, FetchEdgeWeight { inputs });
                }
            }

            self.graph.remove_node(merged);
        }
    }
}

/// A merge_at path with all type conditions removed, for grouping sibling
/// fetches that differ only in which concrete types they apply to.
pub(super) fn strip_merge_at_conditions(
    merge_at: &[FetchDataPathElement],
) -> Vec<FetchDataPathElement> {
    merge_at
        .iter()
        .map(|element| match element {
            FetchDataPathElement::Key(name, _) => FetchDataPathElement::Key(name.clone(), None),
            FetchDataPathElement::AnyIndex(_) => FetchDataPathElement::AnyIndex(None),
            other => other.clone(),
        })
        .collect()
}

impl std::fmt::Display for FetchGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "FetchGraph ({} nodes, {} edges):",
            self.graph.node_count(),
            self.graph.edge_count(),
        )?;
        for idx in self.graph.node_indices() {
            let node = &self.graph[idx];
            let kind_label = match &node.kind {
                FetchGroupKind::Root { root_type } => {
                    format!("root  {}  {}", node.subgraph, root_type)
                }
                FetchGroupKind::Entity { merge_at } => {
                    let path: Vec<String> = merge_at.iter().map(|e| e.to_string()).collect();
                    format!("entity  {}  merge_at={}", node.subgraph, path.join("/"))
                }
                FetchGroupKind::RootHop {
                    root_type,
                    merge_at,
                } => {
                    let path: Vec<String> = merge_at.iter().map(|e| e.to_string()).collect();
                    format!(
                        "root_hop  {}  {}  merge_at={}",
                        node.subgraph,
                        root_type,
                        path.join("/")
                    )
                }
            };
            let sel_count = node.selection_builder.entries().len();
            writeln!(
                f,
                "  [{}] {}  ({} selections)",
                idx.index(),
                kind_label,
                sel_count
            )?;
            for edge in self.graph.edges_directed(idx, Direction::Incoming) {
                writeln!(
                    f,
                    "    <- [{}] ({} inputs)",
                    edge.source().index(),
                    edge.weight().inputs.len(),
                )?;
            }
        }
        Ok(())
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
        assert!(g.root_groups.contains_key(&(sg.clone(), None)));
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
        assert!(!g.root_groups.contains_key(&(sg.clone(), None)));

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

    // --- merge_sibling_entities ---

    #[test]
    fn merge_sibling_entities_merges_same_path_siblings() {
        let mut g = FetchGraph::new();
        let root_sg: Arc<str> = Arc::from("A");
        let sg: Arc<str> = Arc::from("B");
        let root = g.get_or_create_root_group(&root_sg, dummy_root_type());
        // Identical merge_at on both siblings: union_merge_at_conditions takes
        // its all-equal early return.
        let e1 = g.add_entity_group(&sg, user_path(None));
        let e2 = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(root, e1, vec![]);
        g.add_dependency(root, e2, vec![]);

        g.merge_sibling_entities();

        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);
        assert!(g.has_edge(root, e1));
        assert_eq!(g.merge_at(e1), user_path(None).as_slice());
    }

    #[test]
    fn merge_sibling_entities_unions_differing_merge_at_conditions() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("B");
        // Same subgraph and path, differing only in type conditions: siblings
        // merge and the survivor's merge_at is widened to the stripped path.
        let e1 = g.add_entity_group(&sg, user_path(Some(vec![apollo_compiler::name!("Admin")])));
        let _e2 = g.add_entity_group(&sg, user_path(None));

        g.merge_sibling_entities();

        assert_eq!(g.node_count(), 1);
        assert_eq!(g.merge_at(e1), user_path(None).as_slice());
    }

    #[test]
    fn merge_sibling_entities_does_not_merge_dependent_nodes() {
        let mut g = FetchGraph::new();
        let root_sg: Arc<str> = Arc::from("A");
        let sg: Arc<str> = Arc::from("B");
        let root = g.get_or_create_root_group(&root_sg, dummy_root_type());
        // Three same-key siblings where e1 -> e2 is a dependency: e2 must stay
        // separate (merging it would create a cycle), while e3 joins e1's set.
        let e1 = g.add_entity_group(&sg, user_path(None));
        let e2 = g.add_entity_group(&sg, user_path(None));
        let e3 = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(root, e1, vec![]);
        g.add_dependency(root, e3, vec![]);
        g.add_dependency(e1, e2, vec![]);

        g.merge_sibling_entities();

        // root + merged(e1, e3) + e2.
        assert_eq!(g.node_count(), 3);
        assert!(g.has_edge(e1, e2));
        assert!(g.has_edge(root, e1));
        assert!(!g.has_edge(e1, e1));
    }

    #[test]
    fn merge_nodes_into_relocates_edges() {
        let mut g = FetchGraph::new();
        let root_sg: Arc<str> = Arc::from("A");
        let sg: Arc<str> = Arc::from("B");
        let root = g.get_or_create_root_group(&root_sg, dummy_root_type());
        let survivor = g.add_entity_group(&sg, user_path(None));
        let merged = g.add_entity_group(&sg, user_path(None));
        let c1 = g.add_entity_group(&sg, vec![]);
        let c2 = g.add_entity_group(&sg, vec![]);

        // Shared parent (incoming relocation extends the existing root->survivor
        // edge), a child only the merged node had (edge is recreated on the
        // survivor), and a shared child (outgoing relocation extends).
        g.add_dependency(root, survivor, vec![]);
        g.add_dependency(root, merged, vec![]);
        g.add_dependency(merged, c1, vec![]);
        g.add_dependency(survivor, c2, vec![]);
        g.add_dependency(merged, c2, vec![]);
        // Raw edges both ways between survivor and merged exercise the
        // self-edge skips during relocation (raw to avoid depth maintenance
        // rejecting the cycle).
        g.graph
            .add_edge(survivor, merged, FetchEdgeWeight { inputs: vec![] });
        g.graph
            .add_edge(merged, survivor, FetchEdgeWeight { inputs: vec![] });

        g.merge_nodes_into(survivor, &[merged]);

        assert_eq!(g.node_count(), 4); // root, survivor, c1, c2
        assert!(g.has_edge(root, survivor));
        assert!(g.has_edge(survivor, c1));
        assert!(g.has_edge(survivor, c2));
        assert!(!g.has_edge(survivor, survivor));
        assert_eq!(g.edge_count(), 3);
    }

    // --- strip_merge_at_conditions ---

    #[test]
    fn strip_merge_at_conditions_covers_all_variants() {
        let path = vec![
            FetchDataPathElement::Key(
                apollo_compiler::name!("user"),
                Some(vec![apollo_compiler::name!("Admin")]),
            ),
            FetchDataPathElement::AnyIndex(Some(vec![apollo_compiler::name!("Admin")])),
            FetchDataPathElement::TypenameEquals(apollo_compiler::name!("Admin")),
            FetchDataPathElement::Parent,
        ];
        let stripped = strip_merge_at_conditions(&path);
        assert!(matches!(
            &stripped[0],
            FetchDataPathElement::Key(name, None) if name == "user"
        ));
        assert!(matches!(&stripped[1], FetchDataPathElement::AnyIndex(None)));
        assert!(matches!(
            &stripped[2],
            FetchDataPathElement::TypenameEquals(name) if name == "Admin"
        ));
        assert!(matches!(&stripped[3], FetchDataPathElement::Parent));
    }
}
