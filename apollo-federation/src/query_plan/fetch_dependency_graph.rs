use crate::error::FederationError;
use crate::query_graph::graph_path::{OpGraphPathContext, OpPath};
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::QueryGraph;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::operation::NormalizedSelectionSet;
use crate::query_plan::{FetchDataPathElement, QueryPathElement};
use crate::query_plan::{FetchDataRewrite, QueryPlanCost};
use crate::schema::position::{CompositeTypeDefinitionPosition, SchemaRootDefinitionKind};
use crate::schema::ValidFederationSchema;
use apollo_compiler::NodeStr;
use indexmap::{IndexMap, IndexSet};
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use std::sync::Arc;

/// Represents a subgraph fetch of a query plan.
// PORT_NOTE: The JS codebase called this `FetchGroup`, but this naming didn't make it apparent that
// this was a node in a fetch dependency graph, so we've renamed it accordingly.
//
// The JS codebase additionally has a property named `subgraphAndMergeAtKey` that was used as a
// precomputed map key, but this isn't necessary in Rust since we can use `PartialEq`/`Eq`/`Hash`.
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphNode {
    /// The subgraph this fetch is queried against.
    subgraph_name: NodeStr,
    /// Which root operation kind the fetch should have.
    root_kind: SchemaRootDefinitionKind,
    /// The parent type of the fetch's selection set. For fetches against the root, this is the
    /// subgraph's root operation type for the corresponding root kind, but for entity fetches this
    /// will be the subgraph's entity union type.
    parent_type: CompositeTypeDefinitionPosition,
    /// The selection set to be fetched from the subgraph, along with memoized conditions.
    selection_set: FetchSelectionSet,
    /// Whether this fetch uses the federation `_entities` field and correspondingly is against the
    /// subgraph's entity union type (sometimes called a "key" fetch).
    is_entity_fetch: bool,
    /// The inputs to be passed into `_entities` field, if this is an entity fetch.
    inputs: Option<Arc<FetchInputs>>,
    /// Input rewrites for query plan execution to perform prior to executing the fetch.
    input_rewrites: Arc<Vec<Arc<FetchDataRewrite>>>,
    /// As query plan execution runs, it accumulates fetch data into a response object. This is the
    /// path at which to merge in the data for this particular fetch.
    merge_at: Option<Vec<FetchDataPathElement>>,
    /// The fetch ID generation, if one is necessary (used when handling `@defer`).
    id: Option<u64>,
    /// The label of the `@defer` block this fetch appears in, if any.
    defer_ref: Option<NodeStr>,
    /// The cached computation of this fetch's cost, if it's been done already.
    cached_cost: Option<QueryPlanCost>,
    /// Set in some code paths to indicate that the selection set of the group should not be
    /// optimized away even if it "looks" useless.
    must_preserve_selection_set: bool,
    /// If true, then we skip an expensive computation during `is_useless()`. (This partially
    /// caches that computation.)
    is_known_useful: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FetchSelectionSet {
    /// The selection set to be fetched from the subgraph.
    selection_set: Arc<NormalizedSelectionSet>,
    /// The conditions determining whether the fetch should be executed (which must be recomputed
    /// from the selection set when it changes).
    conditions: Conditions,
}

// PORT_NOTE: The JS codebase additionally has a property `onUpdateCallback`. This was only ever
// used to update `isKnownUseful` in `FetchGroup`, and it's easier to handle this there than try
// to pass in a callback in Rust.
#[derive(Debug, Clone)]
pub(crate) struct FetchInputs {
    /// The selection sets to be used as input to `_entities`, separated per parent type.
    selection_sets_per_parent_type:
        IndexMap<CompositeTypeDefinitionPosition, Arc<NormalizedSelectionSet>>,
    /// The supergraph schema (primarily used for validation of added selection sets).
    supergraph_schema: ValidFederationSchema,
}

/// Represents a dependency between two subgraph fetches, namely that the tail/child depends on the
/// head/parent executing first.
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphEdge {
    /// The operation path of the tail/child _relative_ to the head/parent. This information is
    /// maintained in case we want/need to merge groups into each other. This can roughly be thought
    /// of similarly to `merge_at` in the child, but is relative to the start of the parent. It can
    /// be `None`, which either means we don't know the relative path, or that the concept of a
    /// relative path doesn't make sense in this context. E.g. there is case where a child's
    /// `merge_at` can be shorter than its parent's, in which case the `path` (which is essentially
    /// `child.merge_at - parent.merge_at`), does not make sense (or rather, it's negative, which we
    /// cannot represent). The gist is that `None` for the `path` means that no assumption should be
    /// made, and that any merge logic using said path should bail.
    path: Option<Arc<OpPath>>,
}

/// A directed acyclic graph (DAG) of fetches (a.k.a. fetch groups) and their dependencies.
///
/// In the graph, two fetches are connected if one of them (the parent/head) must be performed
/// strictly before the other one (the child/tail).
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraph {
    /// The supergraph schema that generated the federated query graph.
    supergraph_schema: ValidFederationSchema,
    /// The federated query graph that generated the fetches. (This also contains the subgraph
    /// schemas.)
    federated_query_graph: Arc<QueryGraph>,
    /// The nodes/edges of the fetch dependency graph. Note that this must be a stable graph since
    /// we remove nodes/edges during optimizations.
    graph: StableDiGraph<Arc<FetchDependencyGraphNode>, Arc<FetchDependencyGraphEdge>>,
    /// The root nodes by subgraph name, representing the fetches against root operation types of
    /// the subgraphs.
    root_nodes_by_subgraph: IndexMap<NodeStr, NodeIndex>,
    /// Tracks metadata about deferred blocks and their dependencies on one another.
    defer_tracking: Arc<DeferTracking>,
    /// The initial fetch ID generation (used when handling `@defer`).
    starting_id_generation: u64,
    /// The current fetch ID generation (used when handling `@defer`).
    fetch_id_generation: u64,
    /// Whether this fetch dependency graph has undergone a transitive reduction.
    is_reduced: bool,
    /// Whether this fetch dependency graph has undergone optimization (e.g. transitive reduction,
    /// removing empty/useless fetches, merging fetches with the same subgraph/path).
    is_optimized: bool,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct DeferTracking {
    top_level_deferred: IndexSet<NodeStr>,
    deferred: IndexMap<NodeStr, Vec<DeferredInfo>>,
    primary_selection: Option<Arc<NormalizedSelectionSet>>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct DeferredInfo {
    label: NodeStr,
    path: FetchDependencyGraphPath,
    sub_selection: NormalizedSelectionSet,
    deferred: IndexSet<NodeStr>,
    dependencies: IndexSet<NodeStr>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphPath {
    full_path: OpPath,
    path_in_node: OpPath,
    response_path: Vec<FetchDataPathElement>,
}

#[derive(Debug, Default)]
pub(crate) struct FetchDependencyGraphNodePath {
    full_path: Vec<QueryPathElement>,
    path_in_group: Vec<QueryPathElement>,
    response_path: Vec<FetchDataPathElement>,
}

#[derive(Debug)]
pub(crate) struct DeferContext {
    current_defer_ref: Option<NodeStr>,
    path_to_defer_parent: Vec<QueryPathElement>,
    active_defer_ref: Option<NodeStr>,
    is_part_of_query: bool,
}

impl Default for DeferContext {
    fn default() -> Self {
        Self {
            current_defer_ref: None,
            path_to_defer_parent: Vec::new(),
            active_defer_ref: None,
            is_part_of_query: true,
        }
    }
}

impl FetchDependencyGraph {
    pub(crate) fn new(
        supergraph_schema: ValidFederationSchema,
        federated_query_graph: Arc<QueryGraph>,
        root_type_for_defer: Option<CompositeTypeDefinitionPosition>,
        starting_id_generation: u64,
    ) -> Self {
        Self {
            defer_tracking: DeferTracking::empty(&supergraph_schema, root_type_for_defer),
            supergraph_schema,
            federated_query_graph,
            graph: Default::default(),
            root_nodes_by_subgraph: Default::default(),
            starting_id_generation,
            fetch_id_generation: starting_id_generation,
            is_reduced: false,
            is_optimized: false,
        }
    }

    /// Must be called every time the "shape" of the graph is modified
    /// to know that the graph may not be minimal/optimized anymore.
    fn on_modification(&mut self) {
        self.is_reduced = false;
        self.is_optimized = false;
    }

    pub(crate) fn get_or_create_root_node(
        &mut self,
        subgraph_name: &NodeStr,
        root_kind: SchemaRootDefinitionKind,
        parent_type: CompositeTypeDefinitionPosition,
    ) -> Result<NodeIndex, FederationError> {
        if let Some(node) = self.root_nodes_by_subgraph.get(subgraph_name) {
            return Ok(*node);
        }
        let node = self.new_node(
            subgraph_name.clone(),
            parent_type,
            /* has_inputs: */ false,
            root_kind,
        )?;
        self.root_nodes_by_subgraph
            .insert(subgraph_name.clone(), node);
        Ok(node)
    }

    pub(crate) fn new_node(
        &mut self,
        subgraph_name: NodeStr,
        parent_type: CompositeTypeDefinitionPosition,
        has_inputs: bool,
        root_kind: SchemaRootDefinitionKind,
        // merge_at: Option<Vec<ResponsePathElement>>,
        // defer_ref: Option<NodeStr>,
    ) -> Result<NodeIndex, FederationError> {
        let subgraph_schema = self
            .federated_query_graph
            .schema_by_source(&subgraph_name)?
            .clone();
        self.on_modification();
        Ok(self.graph.add_node(Arc::new(FetchDependencyGraphNode {
            subgraph_name: subgraph_name.clone(),
            root_kind,
            selection_set: FetchSelectionSet::empty(subgraph_schema, parent_type.clone())?,
            parent_type,
            is_entity_fetch: has_inputs,
            inputs: has_inputs
                .then(|| Arc::new(FetchInputs::empty(self.supergraph_schema.clone()))),
            input_rewrites: Default::default(),
            merge_at: None,
            id: None,
            defer_ref: None,
            cached_cost: None,
            must_preserve_selection_set: false,
            is_known_useful: false,
        })))
    }
}

impl FetchSelectionSet {
    pub(crate) fn empty(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
    ) -> Result<Self, FederationError> {
        let selection_set = Arc::new(NormalizedSelectionSet::empty(schema, type_position));
        let conditions = selection_set.conditions()?;
        Ok(Self {
            conditions,
            selection_set,
        })
    }
}

impl FetchInputs {
    pub(crate) fn empty(supergraph_schema: ValidFederationSchema) -> Self {
        Self {
            selection_sets_per_parent_type: Default::default(),
            supergraph_schema,
        }
    }
}

impl DeferTracking {
    fn empty(
        schema: &ValidFederationSchema,
        root_type_for_defer: Option<CompositeTypeDefinitionPosition>,
    ) -> Arc<Self> {
        Arc::new(Self {
            top_level_deferred: Default::default(),
            deferred: Default::default(),
            primary_selection: root_type_for_defer.map(|type_position| {
                Arc::new(NormalizedSelectionSet {
                    schema: schema.clone(),
                    type_position,
                    selections: Default::default(),
                })
            }),
        })
    }
}

pub(crate) fn compute_nodes_for_tree(
    _dependency_graph: &mut FetchDependencyGraph,
    _tree: &OpPathTree,
    _start_node: NodeIndex,
    _initial_node_path: FetchDependencyGraphNodePath,
    _initial_defer_context: DeferContext,
    _initial_conditions: OpGraphPathContext,
) -> Vec<NodeIndex> {
    // TODO: port `computeGroupsForTree` in `query-planner-js/src/buildPlan.ts`
    todo!()
}
