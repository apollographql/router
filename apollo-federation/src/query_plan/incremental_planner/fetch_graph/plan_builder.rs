//! Materializing the winning fetch graph into a query plan: nested
//! Sequence/Parallel nodes that start each fetch at its pipeline level, one
//! subgraph operation per fetch group, entity representations from edge
//! inputs, and @defer partitioning.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::executable;
use apollo_compiler::executable::VariableDefinition;
use indexmap::IndexMap;
use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::stable_graph::NodeIndex;
use petgraph::unionfind::UnionFind;
use petgraph::visit::EdgeRef;

use super::super::defer::DeferInfo;
use super::FETCH_COST;
use super::FetchGraph;
use super::FetchGroupKind;
use super::pipelining_factor;
use crate::error::FederationError;
use crate::operation::DirectiveList;
use crate::operation::FieldSelection;
use crate::operation::InlineFragmentSelection;
use crate::operation::Selection;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::VariableCollector;
use crate::query_graph::QueryGraph;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::query_plan::DeferNode;
use crate::query_plan::DeferredDeferBlock;
use crate::query_plan::DeferredDependency;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::PlanNode;
use crate::query_plan::PrimaryDeferBlock;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::conditions::ConditionKind;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::conditions::remove_conditions_from_selection_set;
use crate::query_plan::fetch_dependency_graph::compute_input_rewrites_on_key_fetch;
use crate::query_plan::fetch_dependency_graph::operation_for_entities_fetch;
use crate::query_plan::fetch_dependency_graph::operation_for_query_fetch;
use crate::query_plan::fetch_dependency_graph::wrap_input_selections;
use crate::query_plan::fetch_dependency_graph_processor::to_valid_graphql_name;
use crate::query_plan::query_planner::SubgraphOperationCompression;
use crate::query_plan::requires_selection;
use crate::query_plan::serializable_document::SerializableDocument;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;

/// Everything plan generation needs from the surrounding operation.
pub(crate) struct PlanBuildContext<'a> {
    pub(crate) supergraph_schema: &'a ValidFederationSchema,
    pub(crate) query_graph: &'a Arc<QueryGraph>,
    pub(crate) root_kind: SchemaRootDefinitionKind,
    pub(crate) variable_definitions: &'a [Node<VariableDefinition>],
    pub(crate) operation_directives: &'a DirectiveList,
    pub(crate) operation_name: &'a Option<Name>,
    pub(crate) operation_compression: &'a mut SubgraphOperationCompression,
    /// Numbers generated subgraph operations (`{name}__{subgraph}__{n}`).
    pub(crate) operation_counter: u32,
    /// Numbers fetch nodes referenced by deferred blocks' `depends`; spans
    /// the whole plan so per-field mutation planning cannot collide ids.
    pub(crate) fetch_id_counter: u64,
    /// When true, generated subgraph operations skip document validation
    /// and selection-set validation (valid by construction).
    pub(crate) skip_validation: bool,
}

/// Per-node metadata for materializing one plan's slice of the graph (the
/// whole graph, the primary part, or one deferred block).
struct PlanScope<'a> {
    /// Pipeline depth per node over the whole graph, for cost scaling.
    depth: &'a [u32],
    /// Fetch IDs for defer dependency tracking.
    fetch_ids: Option<&'a HashMap<NodeIndex, u64>>,
}

/// Stamp a fetch ID on the innermost FetchNode: bare, Flatten-wrapped, or
/// gated behind variable @skip/@include Condition wrappers.
fn stamp_fetch_id(plan_node: &mut PlanNode, id: u64) {
    match plan_node {
        PlanNode::Fetch(fetch) => {
            fetch.id = Some(id);
        }
        PlanNode::Flatten(flatten) => {
            stamp_fetch_id(&mut flatten.node, id);
        }
        PlanNode::Condition(condition) => {
            if let Some(node) = condition.if_clause.as_deref_mut() {
                stamp_fetch_id(node, id);
            }
            if let Some(node) = condition.else_clause.as_deref_mut() {
                stamp_fetch_id(node, id);
            }
        }
        _ => {}
    }
}

/// Whether a node with `defer_ref` is planned in the block for `label` or in
/// a block nested inside it.
fn is_within_defer_block<'a>(
    defer_info: &'a DeferInfo,
    mut defer_ref: Option<&'a str>,
    label: &str,
) -> bool {
    while let Some(current) = defer_ref {
        if current == label {
            return true;
        }
        defer_ref = defer_info
            .blocks
            .get(current)
            .and_then(|block| block.parent_label.as_deref());
    }
    false
}

/// Which wavefront fetch's branch a fetch belongs to: the one wavefront
/// fetch it depends on (by position in the wavefront), or none when it
/// depends on several.
#[derive(Clone, Copy, PartialEq)]
enum BranchAssignment {
    Branch(usize),
    Shared,
}

/// Push onto a Sequence child list, splicing a nested Sequence's children
/// in rather than nesting it.
fn push_into_sequence(nodes: &mut Vec<PlanNode>, plan_node: PlanNode) {
    match plan_node {
        PlanNode::Sequence(inner) => nodes.extend(inner.nodes),
        other => nodes.push(other),
    }
}

/// Push onto a Parallel child list, splicing a nested Parallel's children
/// in rather than nesting it.
fn push_into_parallel(nodes: &mut Vec<PlanNode>, plan_node: PlanNode) {
    match plan_node {
        PlanNode::Parallel(inner) => nodes.extend(inner.nodes),
        other => nodes.push(other),
    }
}

/// None for no nodes, the node itself for one, a SequenceNode otherwise.
fn reduce_sequence(mut nodes: Vec<PlanNode>) -> Option<PlanNode> {
    match nodes.len() {
        0 => None,
        1 => nodes.pop(),
        _ => Some(PlanNode::Sequence(crate::query_plan::SequenceNode {
            nodes,
        })),
    }
}

/// None for no nodes, the node itself for one, a ParallelNode otherwise.
fn reduce_parallel(mut nodes: Vec<PlanNode>) -> Option<PlanNode> {
    match nodes.len() {
        0 => None,
        1 => nodes.pop(),
        _ => Some(PlanNode::Parallel(crate::query_plan::ParallelNode {
            nodes,
        })),
    }
}

impl FetchGraph {
    /// Generate a PlanNode tree from the winning fetch graph, wrapped in a
    /// DeferNode when defer info is provided. Each fetch starts at its level,
    /// one past its latest parent, so the plan's depth equals the graph's
    /// critical path. Disconnected parts run in Parallel and never wait on
    /// each other.
    pub(crate) fn to_query_plan_with_defer(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        defer_info: Option<&DeferInfo>,
    ) -> Result<(Option<PlanNode>, QueryPlanCost), FederationError> {
        let depth = self.pipeline_depths()?;

        // Deterministic (dependency-order) node listing; acyclicity was
        // already checked by pipeline_depths.
        let sorted = toposort(&self.graph, None)
            .map_err(|_| FederationError::internal("cycle in FetchGraph"))?;
        if sorted.is_empty() {
            return Ok((None, 0.0));
        }

        // A Defer wrapper is needed whenever the operation has @defer
        // blocks, even if no fetch node carries a defer_ref: a deferred
        // selection whose data rides the primary fetches still needs a
        // data-only block so the router delivers it as a separate chunk.
        let has_defer_blocks = defer_info.is_some_and(|di| !di.blocks.is_empty());

        if !has_defer_blocks {
            return self.plan_for_nodes(ctx, &sorted, &depth, None);
        }

        let defer_info = defer_info.unwrap();

        // Partition nodes by defer_ref.
        let mut primary_nodes: Vec<NodeIndex> = Vec::new();
        let mut deferred_nodes: IndexMap<String, Vec<NodeIndex>> = IndexMap::new();
        for &node_idx in &sorted {
            match &self.graph[node_idx].defer_ref {
                None => primary_nodes.push(node_idx),
                Some(label) => {
                    deferred_nodes
                        .entry(label.clone())
                        .or_default()
                        .push(node_idx);
                }
            }
        }

        // Assign fetch IDs to nodes that parent a node with a different
        // defer_ref (None->Some and Some->Some alike), covering
        // primary-to-deferred and deferred-to-nested-deferred edges.
        let mut node_fetch_ids: HashMap<NodeIndex, u64> = HashMap::new();
        for &node_idx in &sorted {
            let this_defer = self.graph[node_idx].defer_ref.as_deref();
            for edge in self.graph.edges_directed(node_idx, Direction::Outgoing) {
                let child_defer = self.graph[edge.target()].defer_ref.as_deref();
                if child_defer != this_defer {
                    node_fetch_ids.entry(node_idx).or_insert_with(|| {
                        let id = ctx.fetch_id_counter;
                        ctx.fetch_id_counter += 1;
                        id
                    });
                    break;
                }
            }
        }

        // Every label in the operation gets a block, even with no fetch
        // nodes of its own: data riding an enclosing fetch still needs a
        // data-only block (node: None). Labels with fetch nodes come first,
        // in operation order; node-less labels are appended in sorted order.
        let all_labels: Vec<String> = {
            let mut labels: Vec<String> = deferred_nodes.keys().cloned().collect();
            // Synthesized labels follow operation order, so sibling blocks
            // are emitted in the order the client wrote them.
            labels.sort_by_key(|label| {
                defer_info
                    .client_labels
                    .get_index_of(label.as_str())
                    .unwrap_or(usize::MAX)
            });
            let mut node_less: Vec<&String> = defer_info
                .blocks
                .keys()
                .filter(|label| !deferred_nodes.contains_key(*label))
                .collect();
            node_less.sort();
            labels.extend(node_less.into_iter().cloned());
            labels
        };
        let top_level_labels: Vec<String> = all_labels
            .iter()
            .filter(|label| {
                defer_info
                    .blocks
                    .get(label.as_str())
                    .and_then(|bi| bi.parent_label.as_ref())
                    .is_none()
            })
            .cloned()
            .collect();

        // Build child label index: parent_label -> [child_labels]
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for label in &all_labels {
            if let Some(parent) = defer_info
                .blocks
                .get(label.as_str())
                .and_then(|bi| bi.parent_label.as_ref())
            {
                children_of
                    .entry(parent.clone())
                    .or_default()
                    .push(label.clone());
            }
        }

        // Build primary plan.
        let (primary_plan, total_cost) =
            self.plan_for_nodes(ctx, &primary_nodes, &depth, Some(&node_fetch_ids))?;

        // Recursively build deferred blocks.
        let deferred_blocks = self.build_deferred_blocks(
            ctx,
            &top_level_labels,
            &deferred_nodes,
            defer_info,
            &node_fetch_ids,
            &children_of,
            &depth,
        )?;

        let primary_sub_selection = defer_info
            .primary_sub_selection
            .as_deref()
            .map(|s| s.to_owned());

        let defer_node = PlanNode::Defer(DeferNode {
            primary: PrimaryDeferBlock {
                sub_selection: primary_sub_selection,
                node: primary_plan.map(Box::new),
            },
            deferred: deferred_blocks,
        });

        Ok((Some(defer_node), total_cost))
    }

    /// Build a plan for a subset of nodes (the whole graph, the primary
    /// part, or one deferred block's nodes). Parents outside the subset are
    /// treated as already satisfied; they execute in an enclosing or
    /// earlier defer block.
    fn plan_for_nodes(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        nodes: &[NodeIndex],
        depth: &[u32],
        fetch_ids: Option<&HashMap<NodeIndex, u64>>,
    ) -> Result<(Option<PlanNode>, QueryPlanCost), FederationError> {
        if nodes.is_empty() {
            return Ok((None, 0.0));
        }

        // Top-level mutation fields execute serially, but root groups are
        // merged per subgraph, so their interleaving is no longer
        // representable. The entry point plans one top-level field per
        // search, which keeps this to a single root group; fail loudly
        // rather than parallelize if that assumption is ever violated.
        if ctx.root_kind != SchemaRootDefinitionKind::Query {
            let root_groups = nodes.iter().filter(|n| depth[n.index()] == 0).count();
            if root_groups > 1 {
                return Err(FederationError::internal(format!(
                    "cannot order {} root fetch groups under a {} operation",
                    root_groups, ctx.root_kind,
                )));
            }
        }

        let members: HashSet<NodeIndex> = nodes.iter().copied().collect();
        let scope = PlanScope { depth, fetch_ids };

        // Levels count only parents inside this subset; parents outside it
        // run in an enclosing or earlier defer block. `nodes` is in
        // dependency order, so every member parent already has a level.
        let mut levels: HashMap<NodeIndex, u32> = HashMap::with_capacity(nodes.len());
        for &node in nodes {
            let mut level = 0;
            for edge in self.graph.edges_directed(node, Direction::Incoming) {
                let parent = edge.source();
                if !members.contains(&parent) {
                    continue;
                }
                let parent_level = levels.get(&parent).ok_or_else(|| {
                    FederationError::internal("fetch groups are not in dependency order")
                })?;
                level = level.max(parent_level + 1);
            }
            levels.insert(node, level);
        }

        let mut cost: QueryPlanCost = 0.0;
        let plan = self.plan_by_level(ctx, nodes, &levels, &scope, &mut cost)?;
        Ok((plan, cost))
    }

    /// Recursively build `DeferredDeferBlock`s for a set of labels; a label
    /// with children (nested @defer) wraps its fetch nodes and child blocks
    /// in a nested `DeferNode`.
    #[allow(clippy::too_many_arguments)]
    fn build_deferred_blocks(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        labels: &[String],
        deferred_nodes: &IndexMap<String, Vec<NodeIndex>>,
        defer_info: &DeferInfo,
        node_fetch_ids: &HashMap<NodeIndex, u64>,
        children_of: &HashMap<String, Vec<String>>,
        depth: &[u32],
    ) -> Result<Vec<DeferredDeferBlock>, FederationError> {
        let mut blocks: Vec<DeferredDeferBlock> = Vec::new();

        for label in labels {
            // A label with no fetch nodes (data rides an enclosing fetch)
            // is emitted with node: None; the router delivers the chunk
            // from already-fetched data via the block's sub_selection.
            let nodes = deferred_nodes.get(label).map(Vec::as_slice).unwrap_or(&[]);
            let block_info = defer_info.blocks.get(label.as_str());
            let child_labels = children_of.get(label);

            // Dependencies: parent-scope nodes feeding this label's nodes.
            // Parents planned inside this block only run once it starts, so
            // waiting on them would hold the block until the enclosing
            // primary finishes.
            let mut depends: Vec<DeferredDependency> = Vec::new();
            for &deferred_idx in nodes {
                for edge in self.graph.edges_directed(deferred_idx, Direction::Incoming) {
                    let parent_idx = edge.source();
                    if is_within_defer_block(
                        defer_info,
                        self.graph[parent_idx].defer_ref.as_deref(),
                        label,
                    ) {
                        continue;
                    }
                    if let Some(&fetch_id) = node_fetch_ids.get(&parent_idx)
                        && !depends.iter().any(|d| d.id == fetch_id.to_string())
                    {
                        depends.push(DeferredDependency {
                            id: fetch_id.to_string(),
                        });
                    }
                }
            }

            let query_path = block_info
                .map(|bi| bi.query_path.clone())
                .unwrap_or_default();

            // Planning labels are normalization-synthesized; emit the
            // client-visible label they stand for (None when the client
            // left the @defer unlabeled).
            let visible_label = defer_info
                .client_labels
                .get(label.as_str())
                .cloned()
                .flatten();

            let node_plan = if let Some(child_labels) = child_labels
                && !child_labels.is_empty()
            {
                let (inner_plan, _cost) =
                    self.plan_for_nodes(ctx, nodes, depth, Some(node_fetch_ids))?;
                let inner_deferred = self.build_deferred_blocks(
                    ctx,
                    child_labels,
                    deferred_nodes,
                    defer_info,
                    node_fetch_ids,
                    children_of,
                    depth,
                )?;

                // The nested DeferNode's primary carries the block's own
                // (non-deferred) content; the deferred children re-select
                // their pieces via their blocks.
                let nested_defer = PlanNode::Defer(DeferNode {
                    primary: PrimaryDeferBlock {
                        sub_selection: block_info
                            .and_then(|bi| bi.sub_selection.as_deref())
                            .map(|s| s.to_owned()),
                        node: inner_plan.map(Box::new),
                    },
                    deferred: inner_deferred,
                });
                Some(Box::new(nested_defer))
            } else if nodes.is_empty() {
                None
            } else {
                // Leaf blocks still stamp assigned ids: a sibling block can
                // depend on a fetch planned here.
                let (deferred_plan, _cost) =
                    self.plan_for_nodes(ctx, nodes, depth, Some(node_fetch_ids))?;
                deferred_plan.map(Box::new)
            };

            // For nested DeferNodes, sub_selection was consumed above;
            // leaf blocks use it directly.
            let sub_selection = if child_labels.is_some_and(|c| !c.is_empty()) {
                None
            } else {
                block_info
                    .and_then(|bi| bi.sub_selection.as_deref())
                    .map(|s| s.to_owned())
            };

            blocks.push(DeferredDeferBlock {
                depends,
                label: visible_label,
                query_path,
                sub_selection,
                node: node_plan,
            });
        }

        Ok(blocks)
    }

    /// Plan `nodes` so each fetch starts at its level, the length of its
    /// longest chain of parents within the subset. Disconnected parts run in
    /// Parallel, so unrelated branches never wait on each other, and the
    /// plan's depth equals the fetch graph's critical path.
    fn plan_by_level(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        nodes: &[NodeIndex],
        levels: &HashMap<NodeIndex, u32>,
        scope: &PlanScope<'_>,
        cost: &mut QueryPlanCost,
    ) -> Result<Option<PlanNode>, FederationError> {
        let mut parallel = Vec::new();
        for component in self.connected_components(nodes) {
            if let Some(plan_node) = self.plan_component(ctx, &component, levels, scope, cost)? {
                push_into_parallel(&mut parallel, plan_node);
            }
        }
        Ok(reduce_parallel(parallel))
    }

    /// Run a connected part's wavefront (its lowest-level fetches), each
    /// followed by its branch, then plan the rest after them all. See
    /// `assign_branches` for which fetches go in a branch.
    fn plan_component(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        component: &[NodeIndex],
        levels: &HashMap<NodeIndex, u32>,
        scope: &PlanScope<'_>,
        cost: &mut QueryPlanCost,
    ) -> Result<Option<PlanNode>, FederationError> {
        let wavefront_level = component
            .iter()
            .map(|node| levels[node])
            .min()
            .unwrap_or_default();
        let (wavefront, rest): (Vec<NodeIndex>, Vec<NodeIndex>) = component
            .iter()
            .partition(|node| levels[*node] == wavefront_level);
        let (branches, after) = self.assign_branches(&wavefront, &rest, levels);

        let mut parallel = Vec::new();
        for (&wavefront_node, branch) in wavefront.iter().zip(&branches) {
            let mut sequence = Vec::new();
            if let Some(plan_node) = self.materialize(ctx, wavefront_node, scope, cost)? {
                push_into_sequence(&mut sequence, plan_node);
            }
            if let Some(plan_node) = self.plan_by_level(ctx, branch, levels, scope, cost)? {
                push_into_sequence(&mut sequence, plan_node);
            }
            if let Some(plan_node) = reduce_sequence(sequence) {
                push_into_parallel(&mut parallel, plan_node);
            }
        }

        let mut sequence = Vec::new();
        if let Some(plan_node) = reduce_parallel(parallel) {
            push_into_sequence(&mut sequence, plan_node);
        }
        if let Some(plan_node) = self.plan_by_level(ctx, &after, levels, scope, cost)? {
            push_into_sequence(&mut sequence, plan_node);
        }
        Ok(reduce_sequence(sequence))
    }

    /// Assign each fetch after the wavefront to the branch of the one
    /// wavefront fetch it depends on, or run it after every branch.
    ///
    /// ```text
    /// fetches:  w1 -> p1 -> p2    w2 -> q1    p1, q1 -> s
    /// plan:     Parallel(w1 -> p1, w2 -> q1) -> Parallel(s, p2)
    /// ```
    ///
    /// `s` needs both wavefront fetches, so it is shared. Branches stop
    /// below its level, so `p2` waits for the join rather than delaying it.
    fn assign_branches(
        &self,
        wavefront: &[NodeIndex],
        rest: &[NodeIndex],
        levels: &HashMap<NodeIndex, u32>,
    ) -> (Vec<Vec<NodeIndex>>, Vec<NodeIndex>) {
        // `rest` is in dependency order, so a fetch's parents in the part
        // are assigned before it is. Parents outside the part are already
        // done and do not constrain it.
        let mut assignments: HashMap<NodeIndex, BranchAssignment> = wavefront
            .iter()
            .enumerate()
            .map(|(i, node)| (*node, BranchAssignment::Branch(i)))
            .collect();
        for &node in rest {
            let mut assignment = None;
            for edge in self.graph.edges_directed(node, Direction::Incoming) {
                let Some(&parent) = assignments.get(&edge.source()) else {
                    continue;
                };
                assignment = match assignment {
                    None => Some(parent),
                    Some(current) if current == parent => Some(current),
                    Some(_) => Some(BranchAssignment::Shared),
                };
            }
            assignments.insert(node, assignment.unwrap_or(BranchAssignment::Shared));
        }

        let first_shared_level = rest
            .iter()
            .filter(|node| assignments[*node] == BranchAssignment::Shared)
            .map(|node| levels[node])
            .min();
        let mut branches = vec![Vec::new(); wavefront.len()];
        let mut after = Vec::new();
        for &node in rest {
            match assignments[&node] {
                BranchAssignment::Branch(i)
                    if first_shared_level.is_none_or(|shared| levels[&node] < shared) =>
                {
                    branches[i].push(node);
                }
                _ => after.push(node),
            }
        }
        (branches, after)
    }

    /// Weakly connected parts of `nodes` over the edges between them. Parts,
    /// and the nodes within each part, keep `nodes` order.
    fn connected_components(&self, nodes: &[NodeIndex]) -> Vec<Vec<NodeIndex>> {
        let position: HashMap<NodeIndex, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (*node, i))
            .collect();
        let mut parts = UnionFind::new(nodes.len());
        for (i, node) in nodes.iter().enumerate() {
            for edge in self.graph.edges_directed(*node, Direction::Outgoing) {
                if let Some(&j) = position.get(&edge.target()) {
                    parts.union(i, j);
                }
            }
        }

        // Keyed by each part's representative, in order of the part's first
        // node, so the plan keeps a stable order.
        let mut components: IndexMap<usize, Vec<NodeIndex>> = IndexMap::new();
        for (i, node) in nodes.iter().enumerate() {
            components.entry(parts.find_mut(i)).or_default().push(*node);
        }
        components.into_values().collect()
    }

    /// Materialize one node, adding its pipelined cost and its defer fetch
    /// ID.
    fn materialize(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        node_idx: NodeIndex,
        scope: &PlanScope<'_>,
        cost: &mut QueryPlanCost,
    ) -> Result<Option<PlanNode>, FederationError> {
        Ok(self
            .node_to_plan_node(ctx, node_idx)?
            .map(|(mut plan_node, node_cost)| {
                *cost += node_cost * pipelining_factor(scope.depth[node_idx.index()]);
                // Fetch IDs are used for defer dependency tracking.
                if let Some(&fetch_id) = scope.fetch_ids.and_then(|ids| ids.get(&node_idx)) {
                    stamp_fetch_id(&mut plan_node, fetch_id);
                }
                plan_node
            }))
    }

    /// Convert a single FetchGraph node into a PlanNode.
    fn node_to_plan_node(
        &self,
        ctx: &mut PlanBuildContext<'_>,
        node_idx: NodeIndex,
    ) -> Result<Option<(PlanNode, QueryPlanCost)>, FederationError> {
        let node = &self.graph[node_idx];
        let is_entity = matches!(node.kind, FetchGroupKind::Entity { .. });
        // Entity fetches resolve through _entities on the subgraph's Query
        // root regardless of the surrounding operation; a root hop carries
        // its own kind.
        let node_root_kind = match &node.kind {
            FetchGroupKind::Entity { .. } => SchemaRootDefinitionKind::Query,
            FetchGroupKind::Root { .. } => ctx.root_kind,
            FetchGroupKind::RootHop { root_kind, .. } => *root_kind,
        };
        let subgraph_schema = ctx.query_graph.schema_by_source(&node.subgraph)?;

        // Parent type for the selection set; selections are materialized
        // against the subgraph schema (add_at_path rebases supergraph
        // OpPathElements onto it).
        let parent_type: CompositeTypeDefinitionPosition = match &node.kind {
            FetchGroupKind::Root { root_type } | FetchGroupKind::RootHop { root_type, .. } => {
                root_type.clone()
            }
            FetchGroupKind::Entity { .. } => subgraph_schema
                .entity_type()?
                .ok_or_else(|| {
                    FederationError::internal(format!(
                        "Subgraph `{}` has no entities defined",
                        node.subgraph
                    ))
                })?
                .into(),
        };

        // 1. Materialize selection from the builder.
        let mut selection_set = SelectionSet::empty(subgraph_schema.clone(), parent_type.clone());
        for entry in node.selection_builder.entries() {
            let path_vec = entry.path().to_vec();
            selection_set.add_at_path_for_incremental_planner(&path_vec, entry.selections())?;
        }

        if selection_set.selections.is_empty() {
            return Ok(None);
        }

        let selection_cost = selection_set.cost(1.0);
        let node_cost = FETCH_COST + selection_cost;

        // Group-level @skip/@include: when every selection is gated by the
        // same variable conditions, hoist them out of the operation and
        // gate the fetch itself (ConditionNodes below). Execution can then
        // skip the fetch entirely. Unlike the reference planner, no
        // handled-conditions set is threaded through: descendants are
        // sequenced after their parent rather than nested inside its
        // ConditionNode, so every node self-gates.
        let group_conditions = selection_set.conditions()?;
        if let Conditions::Boolean(false) = group_conditions {
            return Ok(None);
        }

        // 2. Finalize selection: strip conditions, flatten, add __typename
        //    and aliases.
        let (finalized_selection, output_rewrites) = Self::finalize_selection(
            &selection_set,
            &group_conditions,
            is_entity,
            &parent_type,
            subgraph_schema,
            ctx.variable_definitions,
            ctx.skip_validation,
        )?;

        // 3. Materialize entity inputs from incoming edges.
        let (requires_selection, input_rewrites) = if is_entity {
            let (sel, rewrites) =
                self.materialize_entity_inputs(ctx, node_idx, &parent_type, &group_conditions)?;
            // Without representations the router can fetch nothing; an
            // entity group reachable only through input-less edges is an
            // upstream bug, not a plannable fetch.
            if sel.selections.is_empty() {
                return Err(FederationError::internal(format!(
                    "entity fetch group for subgraph {} has no representation inputs",
                    node.subgraph,
                )));
            }
            (Some(sel), rewrites)
        } else {
            (None, Vec::new())
        };

        // 4. Collect variable definitions narrowed to those actually used.
        let (variable_definitions, variable_usages) = Self::collect_used_variable_definitions(
            &node.context_variables,
            ctx.variable_definitions,
            ctx.operation_directives,
            &finalized_selection,
        );

        // 5. Build the subgraph operation.
        let op_name = ctx
            .operation_name
            .as_ref()
            .map(|name| {
                let c = ctx.operation_counter;
                ctx.operation_counter += 1;
                let subgraph = to_valid_graphql_name(&node.subgraph).unwrap_or("".into());
                Name::new(&format!("{name}__{subgraph}__{c}"))
                    .map_err(|e| FederationError::internal(e.to_string()))
            })
            .transpose()?;
        let operation = if is_entity {
            operation_for_entities_fetch(
                subgraph_schema,
                finalized_selection,
                variable_definitions,
                ctx.operation_directives,
                &op_name,
            )?
        } else {
            operation_for_query_fetch(
                subgraph_schema,
                node_root_kind,
                finalized_selection,
                variable_definitions,
                ctx.operation_directives,
                &op_name,
            )?
        };
        let operation_document = ctx
            .operation_compression
            .compress(operation, ctx.skip_validation)?;

        // 6. Build requires (trim to the router-expected format).
        let requires = requires_selection
            .as_ref()
            .map(executable::SelectionSet::try_from)
            .transpose()?
            .map(|ss| trim_requires(&ss))
            .unwrap_or_default();

        // 7. Construct FetchNode. Connector fetches carry the connector's
        // synthetic service name, the same name expansion would have given
        // its virtual subgraph, because the router dispatches fetches to
        // connectors by service name.
        let fetch_node = PlanNode::Fetch(Box::new(crate::query_plan::FetchNode {
            subgraph_name: match &node.connector {
                Some(c) => Arc::from(c.id.synthetic_name()),
                None => node.subgraph.clone(),
            },
            protocol: match &node.connector {
                Some(c) => crate::query_plan::FetchProtocol::Connector {
                    coordinate: c.id.coordinate(),
                },
                None => Default::default(),
            },
            id: None,
            variable_usages,
            requires,
            operation_document: SerializableDocument::from_parsed(operation_document),
            operation_name: op_name,
            operation_kind: node_root_kind.into(),
            input_rewrites: Arc::new(input_rewrites),
            output_rewrites,
            context_rewrites: node
                .context_rewrites
                .iter()
                .cloned()
                .map(|r| Arc::new(r.into()))
                .collect(),
        }));

        // 8. Wrap entity/root-hop fetches in FlattenNode.
        let mut plan_node = match &node.kind {
            FetchGroupKind::Entity { merge_at } | FetchGroupKind::RootHop { merge_at, .. } => {
                PlanNode::Flatten(crate::query_plan::FlattenNode {
                    path: merge_at.clone(),
                    node: Box::new(fetch_node),
                })
            }
            FetchGroupKind::Root { .. } => fetch_node,
        };

        // 9. Gate the fetch on its hoisted group conditions.
        if let Conditions::Variables(variables) = &group_conditions {
            for (name, kind) in variables.iter() {
                let (if_clause, else_clause) = match kind {
                    ConditionKind::Skip => (None, Some(Box::new(plan_node))),
                    ConditionKind::Include => (Some(Box::new(plan_node)), None),
                };
                plan_node = PlanNode::Condition(Box::new(crate::query_plan::ConditionNode {
                    condition_variable: name.clone(),
                    if_clause,
                    else_clause,
                }));
            }
        }
        Ok(Some((plan_node, node_cost)))
    }

    /// Strip @skip/@include conditions, flatten unnecessary fragments, add
    /// `__typename` on abstract types, and alias non-merging fields.
    ///
    /// Invariant: an entity fetch's top-level inline fragments are never
    /// flattened, only their contents. Each top-level cast must stay the
    /// exact type its representations name in `__typename` (as produced by
    /// the edge inputs and their rewrites, e.g. an @interfaceObject or
    /// @key'ed interface jump), because the subgraph matches
    /// representations to these casts. Flattening can rewrite a fragment's
    /// type condition, say narrowing an interface cast to its runtime
    /// object types, which would desync the operation's entity cases from
    /// the requires/representations built in materialize_entity_inputs.
    fn finalize_selection(
        selection_set: &SelectionSet,
        group_conditions: &Conditions,
        is_entity: bool,
        parent_type: &CompositeTypeDefinitionPosition,
        subgraph_schema: &ValidFederationSchema,
        variable_definitions: &[Node<VariableDefinition>],
        skip_validation: bool,
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        let stripped = remove_conditions_from_selection_set(selection_set, group_conditions)?;
        let stripped = if is_entity {
            drop_covered_condition_fragments_in_casts(&stripped)?
        } else {
            drop_covered_condition_fragments(&stripped)?
        };
        let selection_without_conditions = if is_entity {
            let mut selections = SelectionMap::new();
            for selection in stripped.selections.values() {
                match selection {
                    Selection::InlineFragment(frag_sel) => {
                        let casted = frag_sel.inline_fragment.casted_type();
                        let flattened = frag_sel
                            .selection_set
                            .flatten_unnecessary_fragments(&casted, subgraph_schema)?;
                        selections.insert(Selection::InlineFragment(Arc::new(
                            InlineFragmentSelection::new(
                                frag_sel.inline_fragment.clone(),
                                flattened,
                            ),
                        )));
                    }
                    other => {
                        selections.insert(other.clone());
                    }
                }
            }
            SelectionSet {
                schema: stripped.schema.clone(),
                type_position: stripped.type_position.clone(),
                selections: Arc::new(selections),
            }
        } else {
            stripped.flatten_unnecessary_fragments(parent_type, subgraph_schema)?
        };
        let selection_with_typenames =
            selection_without_conditions.add_typename_field_for_abstract_types(None)?;
        let (finalized_selection, output_rewrites) =
            selection_with_typenames.add_aliases_for_non_merging_fields()?;
        if !skip_validation {
            finalized_selection.validate(variable_definitions)?;
        }
        Ok((finalized_selection, output_rewrites))
    }

    /// Collect entity representation inputs from an entity group's incoming
    /// edges: the merged requires `SelectionSet` plus the `FetchDataRewrite`
    /// entries for typename and alias rewrites.
    fn materialize_entity_inputs(
        &self,
        ctx: &PlanBuildContext<'_>,
        node_idx: NodeIndex,
        parent_type: &CompositeTypeDefinitionPosition,
        group_conditions: &Conditions,
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        let mut per_type: IndexMap<CompositeTypeDefinitionPosition, SelectionSet> =
            IndexMap::default();
        let mut rewrites: Vec<Arc<FetchDataRewrite>> = Vec::new();

        for edge in self.graph.edges_directed(node_idx, Direction::Incoming) {
            for input in &edge.weight().inputs {
                let input_type: CompositeTypeDefinitionPosition = ctx
                    .supergraph_schema
                    .get_type(input.source_type_name())?
                    .try_into()?;
                let mut input_sel = SelectionSet::for_composite_type(
                    ctx.supergraph_schema.clone(),
                    input_type.clone(),
                );
                input_sel.add_selection_set(input.conditions())?;
                let wrapped = wrap_input_selections(
                    ctx.supergraph_schema,
                    &input_type,
                    input_sel,
                    input
                        .needed_under()
                        .unwrap_or(&OpGraphPathContext::default()),
                );
                // The fetch is gated on the conditions all its selections are
                // under, so the representation needn't check them again.
                let wrapped = remove_conditions_from_selection_set(&wrapped, group_conditions)?;
                let entry = per_type
                    .entry(wrapped.type_position.clone())
                    .or_insert_with(|| {
                        SelectionSet::empty(
                            ctx.supergraph_schema.clone(),
                            wrapped.type_position.clone(),
                        )
                    });
                entry.add_local_selection_set(&wrapped)?;

                if let Some(info) = input.rewrite_info() {
                    let dest_schema = ctx.query_graph.schema_by_source(&info.dest_subgraph)?;
                    if let Some(r) = compute_input_rewrites_on_key_fetch(
                        input.source_type_name(),
                        &info.dest_type,
                        dest_schema,
                    )? {
                        rewrites.extend(r);
                    }
                }
                for (alias, original_name) in input.condition_alias_rewrites() {
                    rewrites.push(Arc::new(FetchDataRewrite::KeyRenamer(
                        crate::query_plan::FetchDataKeyRenamer {
                            path: vec![FetchDataPathElement::Key(
                                alias.clone(),
                                Default::default(),
                            )],
                            rename_key_to: original_name.clone(),
                        },
                    )));
                }
            }
        }

        let mut merged_selections = SelectionMap::new();
        for selection_set in per_type.values() {
            let selection_set = drop_covered_condition_fragments_in_casts(selection_set)?;
            if !ctx.skip_validation {
                selection_set.validate(ctx.variable_definitions)?;
            }
            merged_selections.extend_ref(&selection_set.selections);
        }
        let result = SelectionSet {
            schema: ctx.supergraph_schema.clone(),
            type_position: parent_type.clone(),
            selections: Arc::new(merged_selections),
        };
        Ok((result, rewrites))
    }

    /// Filter the operation's variable definitions to those actually
    /// referenced by the finalized selection and operation directives.
    fn collect_used_variable_definitions(
        context_variables: &[(Name, Node<apollo_compiler::ast::Type>)],
        operation_variable_definitions: &[Node<VariableDefinition>],
        operation_directives: &DirectiveList,
        finalized_selection: &SelectionSet,
    ) -> (Vec<Node<VariableDefinition>>, Vec<Name>) {
        let context_var_defs: Vec<Node<VariableDefinition>> = context_variables
            .iter()
            .map(|(name, ty)| {
                Node::new(VariableDefinition {
                    name: name.clone(),
                    ty: ty.clone(),
                    default_value: None,
                    directives: Default::default(),
                    description: None,
                })
            })
            .collect();
        let all_var_defs: Vec<Node<VariableDefinition>> = operation_variable_definitions
            .iter()
            .cloned()
            .chain(context_var_defs)
            .collect();
        let variable_definitions = {
            let mut collector = VariableCollector::new();
            collector.visit_directive_list(operation_directives);
            collector.visit_selection_set(finalized_selection);
            let used = collector.into_inner();
            all_var_defs
                .iter()
                .filter(|v| used.contains(&v.name))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut variable_usages: Vec<Name> = variable_definitions
            .iter()
            .map(|v| v.name.clone())
            .collect();
        variable_usages.sort();
        (variable_definitions, variable_usages)
    }
}

/// `selection_set` without the condition-only fragments (no type condition,
/// or the parent's own, and only @skip/@include) whose selections it already
/// makes unconditionally, e.g. the `... @include(if: $a) { __typename id }`
/// a gated key input leaves next to an unconditional `__typename id`. Such
/// fragments left with no directives once the fetch's own conditions are
/// removed are inlined.
fn drop_covered_condition_fragments(
    selection_set: &SelectionSet,
) -> Result<SelectionSet, FederationError> {
    let is_condition_only = |selection: &Selection| match selection {
        Selection::InlineFragment(frag_sel) => {
            let fragment = &frag_sel.inline_fragment;
            fragment
                .type_condition_position
                .as_ref()
                .is_none_or(|type_condition| *type_condition == selection_set.type_position)
                && fragment
                    .directives
                    .iter()
                    .all(|d| d.name == "skip" || d.name == "include")
        }
        Selection::Field(_) => false,
    };
    let empty = || {
        SelectionSet::empty(
            selection_set.schema.clone(),
            selection_set.type_position.clone(),
        )
    };
    let mut selections = Vec::with_capacity(selection_set.selections.len());
    let mut unconditional = empty();
    for selection in selection_set.selections.values() {
        let selection = match selection {
            Selection::Field(field_sel) => match &field_sel.selection_set {
                Some(sub) => Selection::Field(Arc::new(FieldSelection {
                    field: field_sel.field.clone(),
                    selection_set: Some(drop_covered_condition_fragments(sub)?),
                })),
                None => selection.clone(),
            },
            Selection::InlineFragment(frag_sel) => {
                Selection::InlineFragment(Arc::new(InlineFragmentSelection::new(
                    frag_sel.inline_fragment.clone(),
                    drop_covered_condition_fragments(&frag_sel.selection_set)?,
                )))
            }
        };
        match &selection {
            Selection::InlineFragment(frag_sel) if is_condition_only(&selection) => {
                if frag_sel.inline_fragment.directives.is_empty() {
                    unconditional.add_local_selection_set(&frag_sel.selection_set)?;
                }
            }
            _ => unconditional.add_local_selection(&selection)?,
        }
        selections.push(selection);
    }
    let mut result = empty();
    for selection in &selections {
        match selection {
            Selection::InlineFragment(frag_sel) if is_condition_only(selection) => {
                if frag_sel.inline_fragment.directives.is_empty() {
                    result.add_local_selection_set(&frag_sel.selection_set)?;
                } else if !unconditional.contains(&frag_sel.selection_set) {
                    result.add_local_selection(selection)?;
                }
            }
            _ => result.add_local_selection(selection)?,
        }
    }
    Ok(result)
}

/// [`drop_covered_condition_fragments`] within the top-level casts of an
/// entity selection, which stay as they are (see `finalize_selection`),
/// except that a cast gated only by @skip/@include is dropped when an
/// ungated cast to the same type already selects what it does.
fn drop_covered_condition_fragments_in_casts(
    selection_set: &SelectionSet,
) -> Result<SelectionSet, FederationError> {
    let mut selections = Vec::with_capacity(selection_set.selections.len());
    for selection in selection_set.selections.values() {
        selections.push(match selection {
            Selection::InlineFragment(frag_sel) => {
                Selection::InlineFragment(Arc::new(InlineFragmentSelection::new(
                    frag_sel.inline_fragment.clone(),
                    drop_covered_condition_fragments(&frag_sel.selection_set)?,
                )))
            }
            Selection::Field(_) => selection.clone(),
        });
    }
    let ungated_casts: Vec<_> = selections
        .iter()
        .filter_map(|selection| match selection {
            Selection::InlineFragment(frag_sel)
                if frag_sel.inline_fragment.directives.is_empty() =>
            {
                Some(frag_sel)
            }
            _ => None,
        })
        .collect();
    let is_covered = |selection: &Selection| match selection {
        Selection::InlineFragment(frag_sel) => {
            let fragment = &frag_sel.inline_fragment;
            !fragment.directives.is_empty()
                && fragment
                    .directives
                    .iter()
                    .all(|d| d.name == "skip" || d.name == "include")
                && ungated_casts.iter().any(|cast| {
                    cast.inline_fragment.type_condition_position == fragment.type_condition_position
                        && cast.selection_set.contains(&frag_sel.selection_set)
                })
        }
        Selection::Field(_) => false,
    };
    let mut result = SelectionMap::new();
    for selection in &selections {
        if !is_covered(selection) {
            result.insert(selection.clone());
        }
    }
    Ok(SelectionSet {
        schema: selection_set.schema.clone(),
        type_position: selection_set.type_position.clone(),
        selections: Arc::new(result),
    })
}

/// Trim a `SelectionSet` from `apollo_compiler::executable` down to the
/// router's `requires_selection` format, discarding fragment spreads.
fn trim_requires(selection_set: &executable::SelectionSet) -> Vec<requires_selection::Selection> {
    selection_set
        .selections
        .iter()
        .filter_map(|s| match s {
            executable::Selection::Field(field) => Some(requires_selection::Selection::Field(
                requires_selection::Field {
                    alias: field.alias.clone(),
                    name: field.name.clone(),
                    selections: trim_requires(&field.selection_set),
                },
            )),
            executable::Selection::InlineFragment(inline) => Some(
                requires_selection::Selection::InlineFragment(requires_selection::InlineFragment {
                    type_condition: inline.type_condition.clone(),
                    conditions: inline
                        .directives
                        .iter()
                        .filter_map(|directive| {
                            requires_selection::Condition::from_directive(directive)
                                .ok()
                                .flatten()
                        })
                        .collect(),
                    selections: trim_requires(&inline.selection_set),
                }),
            ),
            executable::Selection::FragmentSpread(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::super::InputContribution;
    use super::*;
    use crate::Supergraph;
    use crate::query_graph::build_federated_query_graph;
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::position::ObjectTypeDefinitionPosition;
    use crate::subgraph::Subgraph;

    fn setup() -> (ValidFederationSchema, Arc<QueryGraph>) {
        let s1 = Subgraph::parse_and_expand(
            "S1",
            "http://s1",
            r#"
            type Query { t: T }
            type Mutation { m1: Int }
            type T @key(fields: "k") { k: ID }
            "#,
        )
        .expect("S1 parses");
        let s2 = Subgraph::parse_and_expand(
            "S2",
            "http://s2",
            r#"
            type Query { q2: Int }
            type Mutation { m2: Int }
            type T @key(fields: "k") { k: ID a: Int }
            "#,
        )
        .expect("S2 parses");
        let supergraph = Supergraph::compose(vec![&s1, &s2]).expect("composes");
        let api = supergraph
            .to_api_schema(Default::default())
            .expect("api schema");
        let qg = build_federated_query_graph(supergraph.schema.clone(), api, None, None)
            .expect("query graph");
        (supergraph.schema, Arc::new(qg))
    }

    fn mutation_pos() -> CompositeTypeDefinitionPosition {
        CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
            type_name: name!("Mutation"),
        })
    }

    fn append_parsed_selection(
        graph: &mut FetchGraph,
        node: NodeIndex,
        schema: &ValidFederationSchema,
        parent: CompositeTypeDefinitionPosition,
        text: &str,
    ) {
        let selection =
            Arc::new(SelectionSet::parse(schema.clone(), parent, text).expect("selection parses"));
        graph.append_selection(node, &super::super::SharedPath::new(), Some(&selection));
    }

    /// Top-level mutation fields must execute serially, but the fetch graph
    /// merges root groups per subgraph and cannot represent interleaving.
    /// Rather than silently emitting a ParallelNode, the builder must fail.
    #[test]
    fn multiple_mutation_roots_error_instead_of_parallel() {
        let (supergraph_schema, qg) = setup();
        let mut graph = FetchGraph::new();
        let s1: Arc<str> = Arc::from("S1");
        let s2: Arc<str> = Arc::from("S2");
        let r1 = graph.get_or_create_root_group(&s1, mutation_pos());
        let r2 = graph.get_or_create_root_group(&s2, mutation_pos());
        let s1_schema = qg.schema_by_source(&s1).expect("S1 schema").clone();
        let s2_schema = qg.schema_by_source(&s2).expect("S2 schema").clone();
        append_parsed_selection(&mut graph, r1, &s1_schema, mutation_pos(), "m1");
        append_parsed_selection(&mut graph, r2, &s2_schema, mutation_pos(), "m2");

        let mut compression = SubgraphOperationCompression::Disabled;
        let directives = DirectiveList::default();
        let mut ctx = PlanBuildContext {
            supergraph_schema: &supergraph_schema,
            query_graph: &qg,
            root_kind: SchemaRootDefinitionKind::Mutation,
            variable_definitions: &[],
            operation_directives: &directives,
            operation_name: &None,
            operation_compression: &mut compression,
            operation_counter: 0,
            fetch_id_counter: 0,
            skip_validation: false,
        };

        let err = graph
            .to_query_plan_with_defer(&mut ctx, None)
            .expect_err("two mutation root groups cannot be ordered");
        assert!(
            err.to_string().contains("cannot order"),
            "expected the root-ordering error, got: {err}",
        );
    }

    /// An entity group whose incoming edges carry no inputs cannot build
    /// representations; materializing it must error rather than emit an
    /// _entities fetch with an empty requires.
    #[test]
    fn entity_group_without_inputs_errors() {
        let (supergraph_schema, qg) = setup();
        let mut graph = FetchGraph::new();
        let s1: Arc<str> = Arc::from("S1");
        let s2: Arc<str> = Arc::from("S2");
        let root = graph.get_or_create_root_group(&s1, mutation_pos());
        let s1_schema = qg.schema_by_source(&s1).expect("S1 schema").clone();
        let s2_schema = qg.schema_by_source(&s2).expect("S2 schema").clone();
        append_parsed_selection(&mut graph, root, &s1_schema, mutation_pos(), "m1");

        let entity = graph.add_entity_group(
            &s2,
            vec![FetchDataPathElement::Key(name!("t"), Default::default())],
        );
        let entity_parent: CompositeTypeDefinitionPosition = s2_schema
            .entity_type()
            .expect("entity type lookup")
            .expect("S2 has entities")
            .into();
        append_parsed_selection(
            &mut graph,
            entity,
            &s2_schema,
            entity_parent,
            "... on T { a }",
        );
        // Ordering-only edge: no key inputs ever attached.
        graph
            .add_ordering_dependency(root, entity)
            .expect("acyclic");

        let mut compression = SubgraphOperationCompression::Disabled;
        let directives = DirectiveList::default();
        let mut ctx = PlanBuildContext {
            supergraph_schema: &supergraph_schema,
            query_graph: &qg,
            root_kind: SchemaRootDefinitionKind::Mutation,
            variable_definitions: &[],
            operation_directives: &directives,
            operation_name: &None,
            operation_compression: &mut compression,
            operation_counter: 0,
            fetch_id_counter: 0,
            skip_validation: false,
        };
        assert!(
            graph.to_query_plan_with_defer(&mut ctx, None).is_err(),
            "an entity fetch without representations must not materialize",
        );
    }

    /// Entity fetches always target the subgraph's Query root, whatever the
    /// top-level operation kind is; only the root fetch carries the
    /// operation's own kind. Also pins the two-layer materialized shape.
    #[test]
    fn entity_fetch_under_mutation_is_query_kind() {
        let (supergraph_schema, qg) = setup();
        let mut graph = FetchGraph::new();
        let s1: Arc<str> = Arc::from("S1");
        let s2: Arc<str> = Arc::from("S2");
        let root = graph.get_or_create_root_group(&s1, mutation_pos());
        let s1_schema = qg.schema_by_source(&s1).expect("S1 schema").clone();
        let s2_schema = qg.schema_by_source(&s2).expect("S2 schema").clone();
        append_parsed_selection(&mut graph, root, &s1_schema, mutation_pos(), "m1");

        let entity = graph.add_entity_group(
            &s2,
            vec![FetchDataPathElement::Key(name!("t"), Default::default())],
        );
        let entity_parent: CompositeTypeDefinitionPosition = s2_schema
            .entity_type()
            .expect("entity type lookup")
            .expect("S2 has entities")
            .into();
        append_parsed_selection(
            &mut graph,
            entity,
            &s2_schema,
            entity_parent,
            "... on T { a }",
        );

        let t_pos: CompositeTypeDefinitionPosition = supergraph_schema
            .get_type(&name!("T"))
            .expect("T exists")
            .try_into()
            .expect("T is composite");
        let key_conditions = Arc::new(
            SelectionSet::parse(supergraph_schema.clone(), t_pos, "k").expect("key parses"),
        );
        graph.add_dependency(
            root,
            entity,
            vec![InputContribution::Requires {
                source_type_name: name!("T"),
                conditions: key_conditions,
                condition_alias_rewrites: Vec::new(),
            }],
        );

        let mut compression = SubgraphOperationCompression::Disabled;
        let directives = DirectiveList::default();
        let mut ctx = PlanBuildContext {
            supergraph_schema: &supergraph_schema,
            query_graph: &qg,
            root_kind: SchemaRootDefinitionKind::Mutation,
            variable_definitions: &[],
            operation_directives: &directives,
            operation_name: &None,
            operation_compression: &mut compression,
            operation_counter: 0,
            fetch_id_counter: 0,
            skip_validation: false,
        };

        let (plan, cost) = graph
            .to_query_plan_with_defer(&mut ctx, None)
            .expect("plan builds");
        assert!(cost > 0.0);
        let PlanNode::Sequence(seq) = plan.expect("non-empty plan") else {
            panic!("expected a two-stage Sequence");
        };
        assert_eq!(seq.nodes.len(), 2);
        let PlanNode::Fetch(root_fetch) = &seq.nodes[0] else {
            panic!("expected root Fetch first");
        };
        assert_eq!(
            root_fetch.operation_kind,
            executable::OperationType::Mutation,
            "root fetch carries the operation's kind",
        );
        let PlanNode::Flatten(flatten) = &seq.nodes[1] else {
            panic!("expected Flatten(entity fetch) second");
        };
        let PlanNode::Fetch(entity_fetch) = flatten.node.as_ref() else {
            panic!("expected entity Fetch inside Flatten");
        };
        assert_eq!(
            entity_fetch.operation_kind,
            executable::OperationType::Query,
            "entity fetches target the subgraph Query root even under a mutation",
        );
        assert!(
            !entity_fetch.requires.is_empty(),
            "entity fetch carries the key representation requires",
        );
    }

    fn query_pos() -> CompositeTypeDefinitionPosition {
        CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
            type_name: name!("Query"),
        })
    }

    fn query_ctx<'a>(
        supergraph_schema: &'a ValidFederationSchema,
        qg: &'a Arc<QueryGraph>,
        compression: &'a mut SubgraphOperationCompression,
        directives: &'a DirectiveList,
    ) -> PlanBuildContext<'a> {
        PlanBuildContext {
            supergraph_schema,
            query_graph: qg,
            root_kind: SchemaRootDefinitionKind::Query,
            variable_definitions: &[],
            operation_directives: directives,
            operation_name: &None,
            operation_compression: compression,
            operation_counter: 0,
            fetch_id_counter: 0,
            skip_validation: false,
        }
    }

    fn add_keyed_entity_dependency(
        graph: &mut FetchGraph,
        parent: NodeIndex,
        entity: NodeIndex,
        supergraph_schema: &ValidFederationSchema,
    ) {
        let t_pos: CompositeTypeDefinitionPosition = supergraph_schema
            .get_type(&name!("T"))
            .expect("T exists")
            .try_into()
            .expect("T is composite");
        let key_conditions = Arc::new(
            SelectionSet::parse(supergraph_schema.clone(), t_pos, "k").expect("key parses"),
        );
        graph.add_dependency(
            parent,
            entity,
            vec![InputContribution::Requires {
                source_type_name: name!("T"),
                conditions: key_conditions,
                condition_alias_rewrites: Vec::new(),
            }],
        );
    }

    /// A dependency chain must nest under its own root rather than layer
    /// globally: an unrelated root fetch runs in parallel with the whole
    /// chain instead of gating its second link.
    #[test]
    fn independent_branch_does_not_gate_another_branches_chain() {
        let (supergraph_schema, qg) = setup();
        let mut graph = FetchGraph::new();
        let s1: Arc<str> = Arc::from("S1");
        let s2: Arc<str> = Arc::from("S2");
        let s1_schema = qg.schema_by_source(&s1).expect("S1 schema").clone();
        let s2_schema = qg.schema_by_source(&s2).expect("S2 schema").clone();

        let r1 = graph.get_or_create_root_group(&s1, query_pos());
        append_parsed_selection(&mut graph, r1, &s1_schema, query_pos(), "t { k }");
        let r2 = graph.get_or_create_root_group(&s2, query_pos());
        append_parsed_selection(&mut graph, r2, &s2_schema, query_pos(), "q2");

        let entity = graph.add_entity_group(
            &s2,
            vec![FetchDataPathElement::Key(name!("t"), Default::default())],
        );
        let entity_parent: CompositeTypeDefinitionPosition = s2_schema
            .entity_type()
            .expect("entity type lookup")
            .expect("S2 has entities")
            .into();
        append_parsed_selection(
            &mut graph,
            entity,
            &s2_schema,
            entity_parent,
            "... on T { a }",
        );
        add_keyed_entity_dependency(&mut graph, r1, entity, &supergraph_schema);

        let mut compression = SubgraphOperationCompression::Disabled;
        let directives = DirectiveList::default();
        let mut ctx = query_ctx(&supergraph_schema, &qg, &mut compression, &directives);

        let (plan, _cost) = graph
            .to_query_plan_with_defer(&mut ctx, None)
            .expect("plan builds");
        let PlanNode::Parallel(par) = plan.expect("non-empty plan") else {
            panic!("expected top-level Parallel of independent branches");
        };
        assert_eq!(par.nodes.len(), 2);
        let chain = par
            .nodes
            .iter()
            .find_map(|n| match n {
                PlanNode::Sequence(seq) => Some(seq),
                _ => None,
            })
            .expect("one branch is the root->entity Sequence");
        assert_eq!(chain.nodes.len(), 2);
        assert!(matches!(chain.nodes[0], PlanNode::Fetch(_)));
        assert!(matches!(chain.nodes[1], PlanNode::Flatten(_)));
        assert!(
            par.nodes.iter().any(|n| matches!(n, PlanNode::Fetch(_))),
            "the unrelated root fetch is a plain parallel branch",
        );
    }

    /// A node with parents in two different branches is sequenced after
    /// both branches complete.
    #[test]
    fn multi_parent_node_sequences_after_all_parent_branches() {
        let (supergraph_schema, qg) = setup();
        let mut graph = FetchGraph::new();
        let s1: Arc<str> = Arc::from("S1");
        let s2: Arc<str> = Arc::from("S2");
        let s1_schema = qg.schema_by_source(&s1).expect("S1 schema").clone();
        let s2_schema = qg.schema_by_source(&s2).expect("S2 schema").clone();

        let r1 = graph.get_or_create_root_group(&s1, query_pos());
        append_parsed_selection(&mut graph, r1, &s1_schema, query_pos(), "t { k }");
        let r2 = graph.get_or_create_root_group(&s2, query_pos());
        append_parsed_selection(&mut graph, r2, &s2_schema, query_pos(), "q2");

        let entity = graph.add_entity_group(
            &s2,
            vec![FetchDataPathElement::Key(name!("t"), Default::default())],
        );
        let entity_parent: CompositeTypeDefinitionPosition = s2_schema
            .entity_type()
            .expect("entity type lookup")
            .expect("S2 has entities")
            .into();
        append_parsed_selection(
            &mut graph,
            entity,
            &s2_schema,
            entity_parent,
            "... on T { a }",
        );
        add_keyed_entity_dependency(&mut graph, r1, entity, &supergraph_schema);
        graph.add_ordering_dependency(r2, entity).expect("acyclic");

        let mut compression = SubgraphOperationCompression::Disabled;
        let directives = DirectiveList::default();
        let mut ctx = query_ctx(&supergraph_schema, &qg, &mut compression, &directives);

        let (plan, _cost) = graph
            .to_query_plan_with_defer(&mut ctx, None)
            .expect("plan builds");
        let PlanNode::Sequence(seq) = plan.expect("non-empty plan") else {
            panic!("expected Sequence of parents then join node");
        };
        assert_eq!(seq.nodes.len(), 2);
        assert!(
            matches!(&seq.nodes[0], PlanNode::Parallel(par) if par.nodes.len() == 2),
            "both parent roots run in parallel first",
        );
        assert!(
            matches!(&seq.nodes[1], PlanNode::Flatten(_)),
            "the multi-parent entity fetch runs after both",
        );
    }
}
