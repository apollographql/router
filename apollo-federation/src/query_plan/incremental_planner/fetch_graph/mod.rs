//! The fetch graph: fetch groups (nodes), dependencies (edges), and the
//! entity inputs riding those edges, built incrementally during BULB
//! search with checkpoint / undo-log rollback.

pub(crate) mod lookup_builder;
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
use petgraph::visit::NodeIndexable;
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
use crate::schema::position::SchemaRootDefinitionKind;

pub(crate) const FETCH_COST: QueryPlanCost = 1000.0;
pub(crate) const PIPELINING_COST: QueryPlanCost = 100.0;

/// Cost multiplier for a fetch at the given pipeline depth: fetches that
/// wait on longer parent chains cost more.
pub(crate) fn pipelining_factor(depth: u32) -> QueryPlanCost {
    (1.0f64).max(depth as f64 * PIPELINING_COST)
}

#[derive(Clone, Debug)]
pub(crate) enum FetchGroupKind {
    /// Fetch against a subgraph's root operation type, merged at the
    /// response root.
    Root {
        /// The subgraph's root operation type the fetch selects from.
        root_type: CompositeTypeDefinitionPosition,
    },
    /// Entity fetch through `Query._entities`.
    Entity {
        /// Response path where this fetch's results merge into the
        /// overall response.
        merge_at: Vec<FetchDataPathElement>,
    },
    /// Fetch against a subgraph's root operation type whose results merge
    /// at a nested response path, e.g. a root-typed field reached mid-plan.
    RootHop {
        /// The subgraph's root operation type the fetch selects from.
        root_type: CompositeTypeDefinitionPosition,
        /// The root operation kind this hop resolves through. Carried per
        /// hop rather than taken from the surrounding operation: a hop
        /// through a subgraph's query root inside a mutation plan must
        /// still build (and label) a query operation.
        root_kind: SchemaRootDefinitionKind,
        /// Response path where this fetch's results merge into the
        /// overall response.
        merge_at: Vec<FetchDataPathElement>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum InputContribution {
    /// @key fields the parent sends to enter the child; drives input
    /// rewrites on the key fetch.
    Key {
        /// Type of the entity in the parent subgraph the key fields are
        /// selected from.
        source_type_name: Name,
        /// The @key field selections the parent must provide.
        conditions: Arc<SelectionSet>,
        /// Destination type and subgraph, for building the input rewrites.
        rewrite_info: InputRewriteInfo,
    },
    /// @requires condition fields riding an existing edge.
    Requires {
        /// Type of the entity in the parent subgraph the condition fields
        /// are selected from.
        source_type_name: Name,
        /// The @requires field selections the parent must provide.
        conditions: Arc<SelectionSet>,
        /// Alias to original-name pairs for condition fields aliased to
        /// avoid cross-fetch response path collisions; each generates an
        /// input KeyRenamer rewrite undoing the alias before the subgraph
        /// send.
        condition_alias_rewrites: Vec<(Name, Name)>,
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

    pub(crate) fn condition_alias_rewrites(&self) -> &[(Name, Name)] {
        match self {
            Self::Key { .. } => &[],
            Self::Requires {
                condition_alias_rewrites,
                ..
            } => condition_alias_rewrites,
        }
    }
}

#[derive(Clone, Debug)]
/// Where a key input lands, for computing @interfaceObject and renamed-type
/// input rewrites on the child fetch.
pub(crate) struct InputRewriteInfo {
    /// The entity type as the destination subgraph knows it.
    pub(crate) dest_type: CompositeTypeDefinitionPosition,
    /// Name of the subgraph the child fetch targets.
    pub(crate) dest_subgraph: Arc<str>,
}

/// Node weight in the FetchGraph.
#[derive(Clone, Debug)]
pub(crate) struct FetchNode {
    /// Name of the subgraph this fetch is sent to.
    pub(crate) subgraph: Arc<str>,
    /// Whether this is a root fetch, an entity (_entities) fetch, or a
    /// root-typed hop nested inside the response.
    pub(crate) kind: FetchGroupKind,
    /// Accumulates the subgraph operation's selection set, with undo support.
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
    /// Pipeline depth: longest incoming dependency chain. Maintained
    /// incrementally by FetchGraph to avoid per-call toposorts.
    pub(crate) pipeline_depth: u32,
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
            pipeline_depth: 0,
        }
    }

    fn with_defer(mut self, defer_ref: Option<String>) -> Self {
        self.defer_ref = defer_ref;
        self
    }

    fn with_connector(mut self, connector: Arc<crate::connectors::Connector>) -> Self {
        self.connector = Some(connector);
        self
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
    /// The key and requires contributions the parent's response provides as
    /// entity representations for the child fetch. Empty for ordering-only
    /// edges.
    pub(crate) inputs: Vec<InputContribution>,
}

/// A single undoable mutation on the FetchGraph. Logged by mutating
/// methods and replayed in reverse by `rollback()`.
#[derive(Clone, Debug)]
enum FetchGraphOp {
    /// A node was added. Undo: remove_node (StableDiGraph keeps indices
    /// stable).
    AddNode { node_index: NodeIndex },
    /// A node claimed the reuse slot for its group key. Logged directly
    /// after the claiming node's AddNode, so LIFO undo releases the slot
    /// before removing the node. Undo: remove the entry.
    RegisterGroup { key: GroupKey },
    /// An edge was added. Undo: remove_edge.
    AddEdge(EdgeIndex),
    /// An input was appended to an edge. Undo: pop last input.
    AppendEdgeInput(EdgeIndex),
    /// A node's pipeline depth was raised. Undo: restore previous depth and
    /// adjust running_cost.
    DepthChange {
        node_index: NodeIndex,
        old_depth: u32,
    },
    /// A selection was appended to a node. Undo: restore previous head pointer.
    ModifySelection {
        node_index: NodeIndex,
        prev_head: SelectionCheckpoint,
    },
    /// @fromContext rewrites/variables were appended to a node. Undo:
    /// truncate both vecs to their prior lengths.
    AddContext {
        node_index: NodeIndex,
        prev_rewrites: usize,
        prev_variables: usize,
    },
}

/// Reuse-slot key for a fetch group, derived from the node itself by
/// `group_key`. Root groups are shared per (subgraph, defer_ref): a
/// deferred root fetch is a separate group from the primary root in the
/// same subgraph, and one search plans one root kind, so the kind is not
/// part of the key. Entity and root-hop groups are shared per (subgraph,
/// merge_at, defer_ref). One key type and one map keep registration,
/// undo, and lookup on a single mechanism for all three kinds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum GroupKey {
    Root(Arc<str>, Option<String>),
    Entity(Arc<str>, Vec<FetchDataPathElement>, Option<String>),
    RootHop(
        Arc<str>,
        SchemaRootDefinitionKind,
        Vec<FetchDataPathElement>,
        Option<String>,
    ),
}

/// The reuse-slot key for a node. Connector-backed nodes have no key:
/// each connector resolution is its own fetch and must never claim or be
/// found in a reuse slot.
fn group_key(node: &FetchNode) -> Option<GroupKey> {
    if node.connector.is_some() {
        return None;
    }
    Some(match &node.kind {
        FetchGroupKind::Root { .. } => {
            GroupKey::Root(node.subgraph.clone(), node.defer_ref.clone())
        }
        FetchGroupKind::Entity { merge_at } => GroupKey::Entity(
            node.subgraph.clone(),
            merge_at.clone(),
            node.defer_ref.clone(),
        ),
        FetchGroupKind::RootHop {
            root_kind,
            merge_at,
            ..
        } => GroupKey::RootHop(
            node.subgraph.clone(),
            *root_kind,
            merge_at.clone(),
            node.defer_ref.clone(),
        ),
    })
}

/// Opaque undo checkpoint: the undo log length at a point in time.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraphCheckpoint(usize);

/// Lightweight fetch graph for BULB search. Trial branches are applied,
/// scored, and undone on one instance via an append-only mutation log:
/// `checkpoint()` marks a log position, `rollback(cp)` reverses back to it.
#[derive(Clone, Debug)]
pub(crate) struct FetchGraph {
    graph: StableDiGraph<FetchNode, FetchEdgeWeight>,
    /// First-created group per reuse-slot key, so group reuse is a lookup
    /// instead of a node scan. The first node with a key owns the slot;
    /// LIFO undo releases it with that node.
    groups: HashMap<GroupKey, NodeIndex>,
    undo_log: Vec<FetchGraphOp>,
    /// Sum of FETCH_COST * pipelining_factor(depth) over all nodes,
    /// maintained incrementally as nodes and edges are added or rolled back.
    running_cost: QueryPlanCost,
}

impl FetchGraph {
    pub(crate) fn new() -> Self {
        Self {
            graph: StableDiGraph::new(),
            groups: HashMap::new(),
            undo_log: Vec::new(),
            running_cost: 0.0,
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
                FetchGraphOp::AddNode { node_index } => {
                    let depth = self.graph[node_index].pipeline_depth;
                    self.running_cost -= FETCH_COST * pipelining_factor(depth);
                    self.graph.remove_node(node_index);
                }
                FetchGraphOp::RegisterGroup { key } => {
                    self.groups.remove(&key);
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
                FetchGraphOp::AddContext {
                    node_index,
                    prev_rewrites,
                    prev_variables,
                } => {
                    let fetch_node = &mut self.graph[node_index];
                    fetch_node.context_rewrites.truncate(prev_rewrites);
                    fetch_node.context_variables.truncate(prev_variables);
                }
                FetchGraphOp::DepthChange {
                    node_index,
                    old_depth,
                } => {
                    let node = &mut self.graph[node_index];
                    let cur_depth = node.pipeline_depth;
                    node.pipeline_depth = old_depth;
                    self.running_cost -= FETCH_COST * pipelining_factor(cur_depth);
                    self.running_cost += FETCH_COST * pipelining_factor(old_depth);
                }
            }
        }
    }

    /// Add a `FetchNode`, logging for rollback. The node claims its reuse
    /// slot if free; a duplicate for an occupied slot is added unregistered
    /// so the owner's registration survives the duplicate's rollback.
    fn insert_node(&mut self, mut node: FetchNode) -> NodeIndex {
        node.pipeline_depth = 0;
        self.running_cost += FETCH_COST * pipelining_factor(0);
        let key = group_key(&node);
        let id = self.graph.add_node(node);
        self.undo_log.push(FetchGraphOp::AddNode { node_index: id });
        if let Some(key) = key
            && let std::collections::hash_map::Entry::Vacant(slot) = self.groups.entry(key.clone())
        {
            slot.insert(id);
            self.undo_log.push(FetchGraphOp::RegisterGroup { key });
        }
        id
    }

    /// The registered group for a key, if any. Registrations are released
    /// in lock-step with node removal, so a live entry is a live node.
    fn registered_group(&self, key: &GroupKey) -> Option<NodeIndex> {
        let id = *self.groups.get(key)?;
        debug_assert!(
            self.graph.contains_node(id),
            "groups slot points at a removed node; rollback cleanup is broken",
        );
        Some(id)
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
        if let Some(id) =
            self.registered_group(&GroupKey::Root(subgraph.clone(), defer_ref.clone()))
        {
            return id;
        }
        self.insert_node(
            FetchNode::new(subgraph.clone(), FetchGroupKind::Root { root_type })
                .with_defer(defer_ref),
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
            FetchNode::new(subgraph.clone(), FetchGroupKind::Entity { merge_at })
                .with_defer(defer_ref),
        )
    }

    /// Test convenience: a new entity fetch group with no defer scope.
    /// Production callers thread the pending's defer_ref via
    /// add_entity_group_with_defer.
    #[cfg(test)]
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
        root_kind: SchemaRootDefinitionKind,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        self.insert_node(
            FetchNode::new(
                subgraph.clone(),
                FetchGroupKind::RootHop {
                    root_type,
                    root_kind,
                    merge_at,
                },
            )
            .with_defer(defer_ref),
        )
    }

    /// Get or create the root hop group for
    /// (subgraph, root_kind, merge_at, defer_ref).
    pub(crate) fn get_or_create_root_hop_group(
        &mut self,
        subgraph: &Arc<str>,
        root_type: CompositeTypeDefinitionPosition,
        root_kind: SchemaRootDefinitionKind,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let key = GroupKey::RootHop(
            subgraph.clone(),
            root_kind,
            merge_at.clone(),
            defer_ref.clone(),
        );
        if let Some(id) = self.registered_group(&key) {
            return id;
        }
        self.add_root_hop_group(subgraph, root_type, root_kind, merge_at, defer_ref)
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
            FetchNode::new(subgraph.clone(), FetchGroupKind::Root { root_type })
                .with_defer(defer_ref)
                .with_connector(connector),
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
            FetchNode::new(subgraph.clone(), FetchGroupKind::Entity { merge_at })
                .with_defer(defer_ref)
                .with_connector(connector),
        )
    }

    /// Get or create the entity fetch group for (subgraph, merge_at, defer_ref).
    pub(crate) fn get_or_create_entity_group_with_defer(
        &mut self,
        subgraph: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<String>,
    ) -> NodeIndex {
        let key = GroupKey::Entity(subgraph.clone(), merge_at.clone(), defer_ref.clone());
        if let Some(id) = self.registered_group(&key) {
            return id;
        }
        self.add_entity_group_with_defer(subgraph, merge_at, defer_ref)
    }

    /// Whether a directed edge from `parent` to `child` exists.
    pub(crate) fn has_edge(&self, parent: NodeIndex, child: NodeIndex) -> bool {
        self.find_edge(parent, child).is_some()
    }

    /// Find the edge index for a directed edge from `parent` to `child`.
    pub(crate) fn find_edge(&self, parent: NodeIndex, child: NodeIndex) -> Option<EdgeIndex> {
        self.graph.find_edge(parent, child)
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
        // Callers reuse an existing edge rather than adding a parallel one;
        // inputs on a duplicate edge would be invisible to lookups that stop
        // at the first edge.
        debug_assert!(
            !self.has_edge(parent, child),
            "duplicate edge {:?} -> {:?} in the fetch graph",
            parent,
            child,
        );
        let id = self
            .graph
            .add_edge(parent, child, FetchEdgeWeight { inputs });
        self.undo_log.push(FetchGraphOp::AddEdge(id));
        let parent_depth = self.graph[parent].pipeline_depth;
        self.raise_depth(child, parent_depth + 1);
        id
    }

    /// Raise a node's pipeline depth to at least `min_depth`, propagating
    /// increases to all descendants via BFS. Each change is logged for
    /// rollback and adjusts running_cost.
    fn raise_depth(&mut self, node: NodeIndex, min_depth: u32) {
        if self.graph[node].pipeline_depth >= min_depth {
            return;
        }
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((node, min_depth));
        while let Some((n, new_depth)) = queue.pop_front() {
            let cur = &mut self.graph[n];
            if cur.pipeline_depth >= new_depth {
                continue;
            }
            let old_depth = cur.pipeline_depth;
            cur.pipeline_depth = new_depth;
            self.running_cost -= FETCH_COST * pipelining_factor(old_depth);
            self.running_cost += FETCH_COST * pipelining_factor(new_depth);
            self.undo_log.push(FetchGraphOp::DepthChange {
                node_index: n,
                old_depth,
            });
            for edge in self.graph.edges_directed(n, Direction::Outgoing) {
                queue.push_back((edge.target(), new_depth + 1));
            }
        }
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

    /// Iterator over all inputs arriving at `node` from parent fetch
    /// groups.
    pub(crate) fn incoming_inputs(
        &self,
        node: NodeIndex,
    ) -> impl Iterator<Item = &InputContribution> {
        self.graph
            .edges_directed(node, Direction::Incoming)
            .flat_map(|edge| edge.weight().inputs.iter())
    }

    /// Get a reference to an edge's weight.
    pub(crate) fn edge_weight_raw(&self, edge: EdgeIndex) -> &FetchEdgeWeight {
        &self.graph[edge]
    }

    /// Whether adding `new_rewrites` to `edge` would rename two different
    /// aliases to the same original field name.
    pub(crate) fn has_conflicting_condition_rewrites(
        &self,
        edge: EdgeIndex,
        new_rewrites: &[(Name, Name)],
    ) -> bool {
        let existing = &self.graph[edge].inputs;
        for (alias, original) in new_rewrites {
            for input in existing {
                if input
                    .condition_alias_rewrites()
                    .iter()
                    .any(|(a, orig)| orig == original && a != alias)
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
            .filter(|i| i.rewrite_info().is_some())
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

    pub(crate) fn snapshot(&self) -> Self {
        self.clone()
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

    /// Structural cost: FETCH_COST per group, scaled by pipeline depth.
    /// Maintained incrementally by insert_node, add_dependency, and rollback.
    pub(crate) fn cost(&self) -> QueryPlanCost {
        self.running_cost
    }

    /// Pipeline depth (longest parent chain) per node, indexed by node
    /// index. Errors on a cyclic graph.
    pub(crate) fn pipeline_depths(&self) -> Result<Vec<u32>, FederationError> {
        let order = petgraph::algo::toposort(&self.graph, None).map_err(|cycle| {
            let node = cycle.node_id();
            let subgraph = &self.graph[node].subgraph;
            FederationError::internal(format!(
                "cycle in FetchGraph at node {:?} ({})",
                node, subgraph,
            ))
        })?;
        let mut depth = vec![0u32; self.graph.node_bound()];
        for node in order {
            depth[node.index()] = self
                .graph
                .edges_directed(node, Direction::Incoming)
                .map(|e| depth[e.source().index()].saturating_add(1))
                .max()
                .unwrap_or(0);
        }
        Ok(depth)
    }

    /// Whether `to` is reachable from `from` via directed edges.
    pub(crate) fn is_reachable(&self, from: NodeIndex, to: NodeIndex) -> bool {
        petgraph::algo::has_path_connecting(&self.graph, from, to, None)
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
        // Group by (subgraph, defer scope, condition-stripped merge_at).
        // Nodes from different defer scopes never merge: the survivor keeps
        // one defer_ref, which would move the other scope's data into the
        // wrong block. IndexMap for deterministic processing order: when
        // merged groups have edges to each other, order decides how
        // relocated edge inputs interleave, which is visible in the
        // serialized plan.
        #[allow(clippy::type_complexity)]
        let mut groups: IndexMap<
            (Arc<str>, Option<String>, Vec<FetchDataPathElement>),
            Vec<NodeIndex>,
        > = IndexMap::new();
        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];
            // Each connector resolution is its own fetch; merging two would
            // send one connector's fields to the other's endpoint.
            if node.connector.is_some() {
                continue;
            }
            if let FetchGroupKind::Entity { merge_at } = &node.kind {
                let key = (
                    node.subgraph.clone(),
                    node.defer_ref.clone(),
                    strip_merge_at_conditions(merge_at),
                );
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
                    // Earlier merges can connect members that were
                    // independent when the partition was computed, and
                    // merging two sets with paths into each other closes a
                    // cycle. Re-check pairwise reachability on the current
                    // graph and only merge members that are still mutually
                    // unreachable; the rest retry among themselves.
                    let mut remaining = bucket;
                    while remaining.len() > 1 {
                        let mut safe = vec![remaining[0]];
                        let mut rest = Vec::new();
                        for &member in &remaining[1..] {
                            if safe.iter().all(|&kept| {
                                !self.is_reachable(kept, member) && !self.is_reachable(member, kept)
                            }) {
                                safe.push(member);
                            } else {
                                rest.push(member);
                            }
                        }
                        if safe.len() > 1 {
                            let survivor = safe[0];
                            self.union_merge_at_conditions(&safe);
                            self.merge_nodes_into(survivor, &safe[1..]);
                        }
                        remaining = rest;
                    }
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
            signatures: HashMap<Vec<String>, String>,
            merge_at: Option<Vec<FetchDataPathElement>>,
            input_conditions: HashMap<Name, BTreeSet<String>>,
            nodes: Vec<NodeIndex>,
        }
        let mut buckets: Vec<Bucket> = Vec::new();
        let mut unmergeable: Vec<Vec<NodeIndex>> = Vec::new();
        for n in mergeable {
            // A conflicted builder (None) must stay alone; treating it as an
            // empty signature map would make it vacuously compatible with
            // every bucket.
            let Some(signatures) = self.graph[n].selection_builder.field_signatures() else {
                unmergeable.push(vec![n]);
                continue;
            };
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
        buckets
            .into_iter()
            .map(|bucket| bucket.nodes)
            .chain(unmergeable)
            .collect()
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
                    .entry(input.source_type_name().clone())
                    .or_default()
                    .insert(input.conditions().to_string());
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
        // Merging runs once, post-search, so removals and edge relocations
        // bypass the undo log.
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
        assert!(graph.groups.is_empty());
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
            None,
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
            condition_alias_rewrites: Vec::new(),
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
        assert!(g.groups.contains_key(&GroupKey::Root(sg.clone(), None)));
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
        assert!(!g.groups.contains_key(&GroupKey::Root(sg.clone(), None)));

        // Re-creating after rollback should work.
        g.get_or_create_root_group(&sg, dummy_root_type());
        assert_eq!(g.node_count(), 1);
    }

    /// A duplicate node for an occupied slot must not disturb the owner's
    /// registration: the duplicate is added unregistered, and its rollback
    /// leaves the owner both live and findable.
    #[test]
    fn rollback_of_duplicate_keyed_node_keeps_owner_registered() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let key = GroupKey::Root(sg.clone(), None);
        let owner = g.get_or_create_root_group(&sg, dummy_root_type());

        let cp = g.checkpoint();
        let duplicate = g.insert_node(FetchNode::new(
            sg.clone(),
            FetchGroupKind::Root {
                root_type: dummy_root_type(),
            },
        ));
        assert_ne!(owner, duplicate);
        assert_eq!(g.groups.get(&key), Some(&owner));

        g.rollback(cp);
        assert_eq!(g.groups.get(&key), Some(&owner));
        assert_eq!(g.get_or_create_root_group(&sg, dummy_root_type()), owner);
    }

    #[test]
    fn get_or_create_root_hop_group_reuses_same_merge_at() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let kind = SchemaRootDefinitionKind::Query;
        let a = g.get_or_create_root_hop_group(&sg, dummy_root_type(), kind, user_path(None), None);
        let b = g.get_or_create_root_hop_group(&sg, dummy_root_type(), kind, user_path(None), None);
        let c = g.get_or_create_root_hop_group(&sg, dummy_root_type(), kind, vec![], None);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(g.node_count(), 2);
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
        let original = g.get_or_create_entity_group_with_defer(&sg, user_path(None), None);
        g.rollback(cp);

        // Reuse the freed index for an unrelated group (different merge_at).
        let unrelated = g.add_entity_group(&sg, vec![]);
        assert_eq!(
            unrelated.index(),
            original.index(),
            "test setup requires StableDiGraph to reuse the freed index",
        );

        let looked_up = g.get_or_create_entity_group_with_defer(&sg, user_path(None), None);
        assert_ne!(
            looked_up, unrelated,
            "stale groups slot resolved to an unrelated node",
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
    fn merge_sibling_entities_does_not_merge_across_defer_scopes() {
        let mut g = FetchGraph::new();
        let root_sg: Arc<str> = Arc::from("A");
        let sg: Arc<str> = Arc::from("B");
        let root = g.get_or_create_root_group(&root_sg, dummy_root_type());
        // Same subgraph and path but different defer scopes: merging would
        // pull one scope's data into the other's block.
        let primary = g.add_entity_group_with_defer(&sg, user_path(None), None);
        let deferred =
            g.add_entity_group_with_defer(&sg, user_path(None), Some("qp__0".to_string()));
        g.add_dependency(root, primary, vec![]);
        g.add_dependency(root, deferred, vec![]);

        g.merge_sibling_entities();

        assert_eq!(g.node_count(), 3, "defer scopes must not merge");
        assert_eq!(g.node(primary).defer_ref, None);
        assert_eq!(g.node(deferred).defer_ref, Some("qp__0".to_string()));
    }

    #[test]
    fn merge_sibling_entities_does_not_create_cross_set_cycles() {
        let mut g = FetchGraph::new();
        let root_sg: Arc<str> = Arc::from("A");
        let sg_b: Arc<str> = Arc::from("B");
        let sg_c: Arc<str> = Arc::from("C");
        let root = g.get_or_create_root_group(&root_sg, dummy_root_type());
        // Four same-key siblings in B created in the order a1 < b2 < a2 < b1
        // with chains a1 -> x1 -> b1 and a2 -> x2 -> b2 through C. First-fit
        // partitioning forms {a1, b2} and {a2, b1}; merging both sets closes
        // a cycle between the survivors.
        let a1 = g.add_entity_group(&sg_b, user_path(None));
        let b2 = g.add_entity_group(&sg_b, user_path(None));
        let a2 = g.add_entity_group(&sg_b, user_path(None));
        let b1 = g.add_entity_group(&sg_b, user_path(None));
        let x1 = g.add_entity_group(&sg_c, vec![]);
        let x2 = g.add_entity_group(
            &sg_c,
            vec![FetchDataPathElement::Key(
                apollo_compiler::name!("other"),
                Default::default(),
            )],
        );
        g.add_dependency(root, a1, vec![]);
        g.add_dependency(root, a2, vec![]);
        g.add_dependency(a1, x1, vec![]);
        g.add_dependency(x1, b1, vec![]);
        g.add_dependency(a2, x2, vec![]);
        g.add_dependency(x2, b2, vec![]);

        g.merge_sibling_entities();

        g.pipeline_depths()
            .expect("sibling merging must never create a dependency cycle");
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

    #[test]
    fn is_reachable_transitive() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let mid = g.add_entity_group(&sg, vec![]);
        let leaf = g.add_entity_group(&sg, user_path(None));
        g.add_dependency(root, mid, vec![]);
        g.add_dependency(mid, leaf, vec![]);

        assert!(g.is_reachable(root, leaf));
        assert!(g.is_reachable(root, mid));
        assert!(!g.is_reachable(leaf, root));
        assert!(!g.is_reachable(mid, root));
    }

    #[test]
    fn get_or_create_root_group_with_defer_differentiates_labels() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root_type = dummy_root_type();
        let primary = g.get_or_create_root_group(&sg, root_type.clone());
        let deferred = g.get_or_create_root_group_with_defer(
            &sg,
            root_type.clone(),
            Some("label1".to_string()),
        );
        let same_deferred =
            g.get_or_create_root_group_with_defer(&sg, root_type, Some("label1".to_string()));

        assert_ne!(primary, deferred);
        assert_eq!(deferred, same_deferred);
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn add_entity_group_with_defer_sets_defer_ref() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let entity = g.add_entity_group_with_defer(&sg, vec![], Some("d1".to_string()));
        assert_eq!(g.graph[entity].defer_ref.as_deref(), Some("d1"));
    }

    #[test]
    fn get_or_create_entity_group_with_defer_is_idempotent() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let path = user_path(None);
        let e1 = g.get_or_create_entity_group_with_defer(&sg, path.clone(), Some("d1".to_string()));
        let e2 = g.get_or_create_entity_group_with_defer(&sg, path, Some("d1".to_string()));
        assert_eq!(e1, e2);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn edge_has_key_input_returns_false_when_empty() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let entity = g.add_entity_group(&sg, vec![]);
        let edge = g.add_dependency(root, entity, vec![]);
        assert!(!g.edge_has_key_input(edge, &apollo_compiler::name!("User")));
    }

    #[test]
    fn incoming_inputs_empty_for_root() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let root = g.get_or_create_root_group(&sg, dummy_root_type());
        let inputs: Vec<_> = g.incoming_inputs(root).collect();
        assert!(inputs.is_empty());
    }

    #[test]
    fn checkpoint_rollback_root_hop_group() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp = g.checkpoint();
        g.add_root_hop_group(
            &sg,
            dummy_root_type(),
            SchemaRootDefinitionKind::Query,
            vec![],
            None,
        );
        assert_eq!(g.node_count(), 1);
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn checkpoint_rollback_deferred_root_group() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp = g.checkpoint();
        g.get_or_create_root_group_with_defer(&sg, dummy_root_type(), Some("d".to_string()));
        assert_eq!(g.node_count(), 1);
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn checkpoint_rollback_deferred_entity_group() {
        let mut g = FetchGraph::new();
        let sg: Arc<str> = Arc::from("sg");
        let cp = g.checkpoint();
        g.get_or_create_entity_group_with_defer(&sg, user_path(None), Some("d".to_string()));
        assert_eq!(g.node_count(), 1);
        g.rollback(cp);
        assert_eq!(g.node_count(), 0);
    }
}
