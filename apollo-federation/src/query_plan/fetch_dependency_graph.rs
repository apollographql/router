use std::fmt::Write as _;
use std::hash::Hash;
use std::hash::Hasher;
use std::iter;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::OperationType;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable;
use apollo_compiler::executable::VariableDefinition;
use apollo_compiler::name;
use itertools::Itertools;
use petgraph::adj::List as AdjList;
use petgraph::algo::tred::dag_transitive_reduction_closure;
use petgraph::graph::IndexType;
use petgraph::stable_graph::EdgeIndex;
use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::EdgeRef;
use petgraph::visit::IntoEdgeReferences;
use petgraph::visit::IntoNeighbors;
use petgraph::visit::IntoNodeReferences;
use petgraph::visit::NodeIndexable;
use serde::Serialize;

use super::FetchDataKeyRenamer;
use super::query_planner::SubgraphOperationCompression;
use crate::bail;
use crate::display_helpers::DisplayOption;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::graphql_definition::DeferDirectiveArguments;
use crate::operation::ArgumentList;
use crate::operation::ContainmentOptions;
use crate::operation::DirectiveList;
use crate::operation::Field;
use crate::operation::HasSelectionKey;
use crate::operation::InlineFragment;
use crate::operation::InlineFragmentSelection;
use crate::operation::Operation;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionMap;
use crate::operation::SelectionSet;
use crate::operation::TYPENAME_FIELD;
use crate::operation::VariableCollector;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::query_graph::graph_path::operation::OpGraphPathTrigger;
use crate::query_graph::graph_path::operation::OpPath;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::query_graph::graph_path::operation::concat_op_paths;
use crate::query_graph::graph_path::operation::concat_paths_in_parents;
use crate::query_graph::path_tree::OpPathTree;
use crate::query_graph::path_tree::PathTreeChild;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchDataValueSetter;
use crate::query_plan::QueryPlanCost;
use crate::query_plan::conditions::Conditions;
use crate::query_plan::conditions::remove_conditions_from_selection_set;
use crate::query_plan::conditions::remove_unneeded_top_level_fragment_directives;
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphProcessor;
use crate::query_plan::requires_selection;
use crate::query_plan::serializable_document::SerializableDocument;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::PositionLookupError;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::TypeDefinitionPosition;
use crate::subgraph::spec::ANY_SCALAR_NAME;
use crate::subgraph::spec::ENTITIES_QUERY;
use crate::supergraph::FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME;
use crate::supergraph::FEDERATION_REPRESENTATIONS_VAR_NAME;
use crate::utils::MultiIndexMap;
use crate::utils::iter_into_single_item;
use crate::utils::logging::snapshot;

/// Represents the value of a `@defer(label:)` argument.
type DeferRef = String;

/// Map of defer labels to nodes of the fetch dependency graph.
///
/// Like a multimap with a Set instead of a Vec for value storage.
#[derive(Debug, Clone, Default)]
struct DeferredNodes {
    inner: IndexMap<DeferRef, IndexSet<NodeIndex<u32>>>,
}
impl DeferredNodes {
    fn new() -> Self {
        Self::default()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn insert(&mut self, defer_ref: DeferRef, node: NodeIndex<u32>) {
        self.inner.entry(defer_ref).or_default().insert(node);
    }

    fn get_all<'map>(&'map self, defer_ref: &DeferRef) -> Option<&'map IndexSet<NodeIndex<u32>>> {
        self.inner.get(defer_ref)
    }

    fn iter(&self) -> impl Iterator<Item = (&'_ DeferRef, NodeIndex<u32>)> {
        self.inner
            .iter()
            .flat_map(|(defer_ref, nodes)| iter::repeat(defer_ref).zip(nodes.iter().copied()))
    }

    /// Consume the map and yield each element. This is provided as a standalone method and not an
    /// `IntoIterator` implementation because it's hard to type :)
    fn into_iter(self) -> impl Iterator<Item = (DeferRef, NodeIndex<u32>)> {
        self.inner.into_iter().flat_map(|(defer_ref, nodes)| {
            // Cloning the key is a bit wasteful, but keys are typically very small,
            // and this map is also very small.
            iter::repeat_with(move || defer_ref.clone()).zip(nodes)
        })
    }
}
impl Extend<(DeferRef, NodeIndex<u32>)> for DeferredNodes {
    fn extend<T: IntoIterator<Item = (DeferRef, NodeIndex<u32>)>>(&mut self, iter: T) {
        for (defer_ref, node) in iter.into_iter() {
            self.insert(defer_ref, node);
        }
    }
}
impl FromIterator<(DeferRef, NodeIndex<u32>)> for DeferredNodes {
    fn from_iter<T: IntoIterator<Item = (DeferRef, NodeIndex<u32>)>>(iter: T) -> Self {
        let mut nodes = Self::new();
        nodes.extend(iter);
        nodes
    }
}
impl FromIterator<(DeferRef, IndexSet<NodeIndex<u32>>)> for DeferredNodes {
    fn from_iter<T: IntoIterator<Item = (DeferRef, IndexSet<NodeIndex<u32>>)>>(iter: T) -> Self {
        let inner = iter.into_iter().collect();
        Self { inner }
    }
}

/// Represents a subgraph fetch of a query plan.
// PORT_NOTE: The JS codebase called this `FetchGroup`, but this naming didn't make it apparent that
// this was a node in a fetch dependency graph, so we've renamed it accordingly.
//
// The JS codebase additionally has a property named `subgraphAndMergeAtKey` that was used as a
// precomputed map key, but this isn't necessary in Rust since we can use `PartialEq`/`Eq`/`Hash`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FetchDependencyGraphNode {
    /// The subgraph this fetch is queried against.
    pub(crate) subgraph_name: Arc<str>,
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
    /// Rewrites that will need to occur to store contextual data for future use
    context_inputs: Vec<FetchDataKeyRenamer>,
    /// As query plan execution runs, it accumulates fetch data into a response object. This is the
    /// path at which to merge in the data for this particular fetch.
    merge_at: Option<Vec<FetchDataPathElement>>,
    /// Precomputed hash of (subgraph_name, merge_at, defer_ref) for fast-reject in
    /// `can_merge_sibling_in`. Not used in `can_merge_grand_child_in` because that
    /// method compares fields from mixed nodes (child vs parent).
    #[serde(skip)]
    merge_key_hash: u64,
    /// The fetch ID generation, if one is necessary (used when handling `@defer`).
    ///
    /// This can be treated as an Option using `OnceLock::get()`.
    #[serde(skip)]
    id: OnceLock<u64>,
    /// The label of the `@defer` block this fetch appears in, if any.
    defer_ref: Option<DeferRef>,
    /// The cached computation of this fetch's cost, if it's been done already.
    cached_cost: Option<QueryPlanCost>,
    /// Set in some code paths to indicate that the selection set of the node should not be
    /// optimized away even if it "looks" useless.
    must_preserve_selection_set: bool,
    /// If true, then we skip an expensive computation during `is_useless()`. (This partially
    /// caches that computation.)
    is_known_useful: bool,
}

fn compute_merge_key_hash(
    subgraph_name: &str,
    merge_at: &Option<Vec<FetchDataPathElement>>,
    defer_ref: &Option<DeferRef>,
) -> u64 {
    let mut hasher = ahash::AHasher::default();
    subgraph_name.hash(&mut hasher);
    merge_at.hash(&mut hasher);
    defer_ref.hash(&mut hasher);
    hasher.finish()
}

/// Safely generate IDs for fetch dependency nodes without mutable access.
#[derive(Debug)]
pub(crate) struct FetchIdGenerator {
    next: AtomicU64,
}
impl FetchIdGenerator {
    /// Create an ID generator, starting at the given value.
    pub(crate) fn new() -> Self {
        Self {
            next: AtomicU64::new(0),
        }
    }

    /// Generate a new ID for a fetch dependency node.
    pub(crate) fn next_id(&self) -> u64 {
        self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FetchSelectionSet {
    /// The selection set to be fetched from the subgraph.
    pub(crate) selection_set: Arc<SelectionSet>,
    /// The conditions determining whether the fetch should be executed, derived from the selection
    /// set.
    #[serde(skip)]
    conditions: OnceLock<Conditions>,
}

// PORT_NOTE: The JS codebase additionally has a property `onUpdateCallback`. This was only ever
// used to update `isKnownUseful` in `FetchGroup`, and it's easier to handle this there than try
// to pass in a callback in Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FetchInputs {
    /// The selection sets to be used as input to `_entities`, separated per parent type.
    selection_sets_per_parent_type: IndexMap<CompositeTypeDefinitionPosition, Arc<SelectionSet>>,
    /// The supergraph schema (primarily used for validation of added selection sets).
    #[serde(skip)]
    supergraph_schema: ValidFederationSchema,
    /// Contexts used as inputs
    used_contexts: IndexMap<Name, Node<Type>>,
}

/// Represents a dependency between two subgraph fetches, namely that the tail/child depends on the
/// head/parent executing first.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FetchDependencyGraphEdge {
    /// The operation path of the tail/child _relative_ to the head/parent. This information is
    /// maintained in case we want/need to merge nodes into each other. This can roughly be thought
    /// of similarly to `merge_at` in the child, but is relative to the start of the parent. It can
    /// be `None`, which either means we don't know the relative path, or that the concept of a
    /// relative path doesn't make sense in this context. E.g. there is case where a child's
    /// `merge_at` can be shorter than its parent's, in which case the `path` (which is essentially
    /// `child.merge_at - parent.merge_at`), does not make sense (or rather, it's negative, which we
    /// cannot represent). The gist is that `None` for the `path` means that no assumption should be
    /// made, and that any merge logic using said path should bail.
    path: Option<Arc<OpPath>>,
}

type FetchDependencyGraphPetgraph =
    StableDiGraph<Arc<FetchDependencyGraphNode>, Arc<FetchDependencyGraphEdge>>;

/// A directed acyclic graph (DAG) of fetches (a.k.a. fetch groups) and their dependencies.
///
/// In the graph, two fetches are connected if one of them (the parent/head) must be performed
/// strictly before the other one (the child/tail).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FetchDependencyGraph {
    /// The supergraph schema that generated the federated query graph.
    #[serde(skip)]
    pub(crate) supergraph_schema: ValidFederationSchema,
    /// The federated query graph that generated the fetches. (This also contains the subgraph
    /// schemas.)
    #[serde(skip)]
    federated_query_graph: Arc<QueryGraph>,
    /// The nodes/edges of the fetch dependency graph. Note that this must be a stable graph since
    /// we remove nodes/edges during optimizations.
    graph: FetchDependencyGraphPetgraph,
    /// The root nodes by subgraph name, representing the fetches against root operation types of
    /// the subgraphs.
    root_nodes_by_subgraph: IndexMap<Arc<str>, NodeIndex>,
    /// Tracks metadata about deferred blocks and their dependencies on one another.
    // TODO(@TylerBloom): Since defer is not supported yet. Once it is, having this field in the
    // serialized output will be needed.
    #[serde(skip)]
    pub(crate) defer_tracking: DeferTracking,
    /// The current fetch ID generation (used when handling `@defer`).
    #[serde(skip)]
    pub(crate) fetch_id_generation: Arc<FetchIdGenerator>,
    /// Whether this fetch dependency graph has undergone a transitive reduction.
    is_reduced: bool,
    /// Whether this fetch dependency graph has undergone optimization (e.g. transitive reduction,
    /// removing empty/useless fetches, merging fetches with the same subgraph/path).
    is_optimized: bool,
    /// Edges that were removed during transitive reduction but crossed a defer boundary
    /// (source.defer_ref != target.defer_ref). These are tracked so that
    /// `extract_children_and_deferred_dependencies` can still register the correct defer
    /// dependencies even after the direct edge has been removed from the graph.
    ///
    /// These `NodeIndex` values are live only until subsequent optimization passes
    /// (`remove_useless_nodes`, `merge_*`) run. Those passes must keep this list in sync:
    /// a node truly removed has no data, so the entry can be dropped; a node merged
    /// into another keeps its data under the surviving index, so the entry is remapped.
    #[serde(skip)]
    reduced_defer_edges: Vec<(NodeIndex, NodeIndex)>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct DeferTracking {
    pub(crate) top_level_deferred: IndexSet<DeferRef>,
    pub(crate) deferred: IndexMap<DeferRef, DeferredInfo>,
    pub(crate) primary_selection: Option<SelectionSet>,
}

// TODO: Write docstrings
// TODO(@goto-bus-stop): this does not seem like it should be cloned around
#[derive(Debug, Clone)]
pub(crate) struct DeferredInfo {
    pub(crate) label: DeferRef,
    pub(crate) path: FetchDependencyGraphNodePath,
    pub(crate) sub_selection: SelectionSet,
    pub(crate) deferred: IndexSet<DeferRef>,
    pub(crate) dependencies: IndexSet<DeferRef>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphNodePath {
    schema: ValidFederationSchema,
    pub(crate) full_path: Arc<OpPath>,
    path_in_node: Arc<OpPath>,
    response_path: Vec<FetchDataPathElement>,
    type_conditioned_fetching_enabled: bool,
    possible_types: IndexSet<Name>,
    possible_types_after_last_field: IndexSet<Name>,
}

#[derive(Debug, Clone)]
pub(crate) struct DeferContext {
    current_defer_ref: Option<DeferRef>,
    path_to_defer_parent: Arc<OpPath>,
    active_defer_ref: Option<DeferRef>,
    is_part_of_query: bool,
}

/// Used in `FetchDependencyGraph` to store, for a given node, information about one of its parent.
/// Namely, this structure stores:
/// 1. the actual parent node index, and
/// 2. the path of the node for which this is a "parent relation" into said parent (`path_in_parent`). This information
///    is maintained for the case where we want/need to merge nodes into each other. One can roughly think of
///    this as similar to a `mergeAt`, but that is relative to the start of `group`. It can be `None`, which
///    either mean we don't know that path or that this simply doesn't make sense (there is case where a child `mergeAt` can
///    be shorter than its parent's, in which case the `path`, which is essentially `child-mergeAt - parent-mergeAt`, does
///    not make sense (or rather, it's negative, which we cannot represent)). Tl;dr, `None` for the `path` means that
///    should make no assumption and bail on any merging that uses said path.
// PORT_NOTE: In JS this uses reference equality, not structural equality, so maybe we should just
// do pointer comparisons?
#[derive(Debug, Clone, PartialEq)]
struct ParentRelation {
    parent_node_id: NodeIndex,
    path_in_parent: Option<Arc<OpPath>>,
}

/// UnhandledNode is used while processing fetch nodes in dependency order to track nodes for which
/// one of the parents has been processed/handled but which has other parents.
// PORT_NOTE: In JS this was a tuple
#[derive(Debug)]
struct UnhandledNode {
    /// The unhandled node.
    node: NodeIndex,
    /// The parents that still need to be processed before the node can be.
    unhandled_parents: Vec<ParentRelation>,
}

impl std::fmt::Display for UnhandledNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (missing: [", self.node.index(),)?;
        for (i, unhandled) in self.unhandled_parents.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", unhandled.parent_node_id.index())?;
        }
        write!(f, "])")
    }
}

/// Used during the processing of fetch nodes in dependency order.
#[derive(Debug)]
struct ProcessingState {
    /// Nodes that can be handled (because all their parents/dependencies have been processed before).
    // TODO(@goto-bus-stop): Seems like this should be an IndexSet, since every `.push()` first
    // checks if the element is unique.
    pub(crate) next: Vec<NodeIndex>,
    /// Nodes that needs some parents/dependencies to be processed first before they can be themselves.
    /// Note that we make sure that this never hold node with no "edges".
    pub(crate) unhandled: Vec<UnhandledNode>,
}

impl DeferContext {
    fn after_subgraph_jump(&self) -> Self {
        Self {
            active_defer_ref: self.current_defer_ref.clone(),
            // Clone the rest as-is
            current_defer_ref: self.current_defer_ref.clone(),
            path_to_defer_parent: self.path_to_defer_parent.clone(),
            is_part_of_query: self.is_part_of_query,
        }
    }
}

impl Default for DeferContext {
    fn default() -> Self {
        Self {
            current_defer_ref: None,
            path_to_defer_parent: Default::default(),
            active_defer_ref: None,
            is_part_of_query: true,
        }
    }
}

impl ProcessingState {
    pub(crate) fn empty() -> Self {
        Self {
            next: vec![],
            unhandled: vec![],
        }
    }

    pub(crate) fn of_ready_nodes(nodes: Vec<NodeIndex>) -> Self {
        Self {
            next: nodes,
            unhandled: vec![],
        }
    }

    // PORT_NOTE: `forChildrenOfProcessedNode` is moved into the FetchDependencyGraph
    // structure as `create_state_for_children_of_processed_node`, because it needs access to the
    // graph.

    pub(crate) fn merge_with(self, other: ProcessingState) -> ProcessingState {
        let mut next = self.next;
        for g in other.next {
            if !next.contains(&g) {
                next.push(g);
            }
        }

        let mut unhandled = vec![];
        let mut that_unhandled = other.unhandled;

        fn merge_remains_and_remove_if_found(
            node_index: NodeIndex,
            mut in_edges: Vec<ParentRelation>,
            other_nodes: &mut Vec<UnhandledNode>,
        ) -> Vec<ParentRelation> {
            let Some((other_index, other_node)) = other_nodes
                .iter()
                .enumerate()
                .find(|(_index, other)| other.node == node_index)
            else {
                return in_edges;
            };

            // The uhandled are the one that are unhandled on both side.
            in_edges.retain(|e| other_node.unhandled_parents.contains(e));
            other_nodes.remove(other_index);
            in_edges
        }

        for node in self.unhandled {
            let new_edges = merge_remains_and_remove_if_found(
                node.node,
                node.unhandled_parents,
                &mut that_unhandled,
            );
            if new_edges.is_empty() {
                if !next.contains(&node.node) {
                    next.push(node.node)
                }
            } else {
                unhandled.push(UnhandledNode {
                    node: node.node,
                    unhandled_parents: new_edges,
                });
            }
        }

        // Anything remaining in `thatUnhandled` are nodes that were not in `self` at all.
        unhandled.extend(that_unhandled);

        ProcessingState { next, unhandled }
    }

    pub(crate) fn update_for_processed_nodes(self, processed: &[NodeIndex]) -> ProcessingState {
        let mut next = self.next;
        let mut unhandled = vec![];
        for UnhandledNode {
            node: g,
            unhandled_parents: mut edges,
        } in self.unhandled
        {
            // Remove any of the processed nodes from the unhandled edges of that node.
            // And if there is no remaining edge, that node can be handled.
            edges.retain(|edge| !processed.contains(&edge.parent_node_id));
            if edges.is_empty() {
                if !next.contains(&g) {
                    next.push(g);
                }
            } else {
                unhandled.push(UnhandledNode {
                    node: g,
                    unhandled_parents: edges,
                });
            }
        }
        ProcessingState { next, unhandled }
    }
}

impl FetchDependencyGraphNodePath {
    pub(crate) fn new(
        schema: ValidFederationSchema,
        type_conditioned_fetching_enabled: bool,
        root_type: CompositeTypeDefinitionPosition,
    ) -> Result<Self, FederationError> {
        let root_possible_types: IndexSet<Name> = if type_conditioned_fetching_enabled {
            schema.possible_runtime_types(root_type)?
        } else {
            Default::default()
        }
        .into_iter()
        .map(|pos| Ok::<_, PositionLookupError>(pos.get(schema.schema())?.name.clone()))
        .process_results(|c| c.sorted().collect())?;

        Ok(Self {
            schema,
            type_conditioned_fetching_enabled,
            full_path: Default::default(),
            path_in_node: Default::default(),
            response_path: Default::default(),
            possible_types: root_possible_types.clone(),
            possible_types_after_last_field: root_possible_types,
        })
    }
    fn for_new_key_fetch(&self, new_context: Arc<OpPath>) -> Self {
        Self {
            schema: self.schema.clone(),
            full_path: self.full_path.clone(),
            path_in_node: new_context,
            response_path: self.response_path.clone(),
            type_conditioned_fetching_enabled: self.type_conditioned_fetching_enabled,
            possible_types: self.possible_types.clone(),
            possible_types_after_last_field: self.possible_types_after_last_field.clone(),
        }
    }

    fn add(
        &self,
        element: Arc<OpPathElement>,
    ) -> Result<FetchDependencyGraphNodePath, FederationError> {
        let response_path = self.updated_response_path(&element)?;
        let new_possible_types = self.new_possible_types(&element)?;
        let possible_types_after_last_field = if let &OpPathElement::Field(_) = element.as_ref() {
            new_possible_types.clone()
        } else {
            self.possible_types_after_last_field.clone()
        };

        Ok(Self {
            schema: self.schema.clone(),
            response_path,
            full_path: Arc::new(self.full_path.with_pushed(element.clone())),
            path_in_node: Arc::new(self.path_in_node.with_pushed(element)),
            type_conditioned_fetching_enabled: self.type_conditioned_fetching_enabled,
            possible_types: new_possible_types,
            possible_types_after_last_field,
        })
    }

    fn new_possible_types(
        &self,
        element: &OpPathElement,
    ) -> Result<IndexSet<Name>, FederationError> {
        if !self.type_conditioned_fetching_enabled {
            return Ok(Default::default());
        }

        let res = match element {
            OpPathElement::InlineFragment(f) => match &f.type_condition_position {
                None => self.possible_types.clone(),
                Some(tcp) => {
                    let element_possible_types = self.schema.possible_runtime_types(tcp.clone())?;
                    self.possible_types
                        .iter()
                        .filter(|&possible_type| {
                            element_possible_types
                                .contains(&ObjectTypeDefinitionPosition::new(possible_type.clone()))
                        })
                        .cloned()
                        .collect()
                }
            },
            OpPathElement::Field(f) => self.advance_field_type(f)?,
        };
        Ok(res)
    }

    fn advance_field_type(&self, element: &Field) -> Result<IndexSet<Name>, FederationError> {
        if !element.output_base_type()?.is_composite_type() {
            return Ok(Default::default());
        }

        let mut res: IndexSet<Name> = self
            .possible_types
            .iter()
            .map(|pt| {
                let field = CompositeTypeDefinitionPosition::try_from(self.schema.get_type(pt)?)?
                    .field(element.name().clone())?
                    .get(self.schema.schema())?;
                let typ = self
                    .schema
                    .get_type(field.ty.inner_named_type())?
                    .try_into()?;
                Ok(self
                    .schema
                    .possible_runtime_types(typ)?
                    .into_iter()
                    .map(|ctdp| ctdp.type_name))
            })
            .process_results::<_, _, FederationError, _>(|c| c.flatten().collect())?;

        res.sort();

        Ok(res)
    }

    fn updated_response_path(
        &self,
        element: &OpPathElement,
    ) -> Result<Vec<FetchDataPathElement>, FederationError> {
        let mut new_path = self.response_path.clone();
        match element {
            OpPathElement::InlineFragment(_) => Ok(new_path),
            OpPathElement::Field(field) => {
                // Type conditions on the last element of a path don't imply different subgraph fetches.
                // They would only translate to a potentially new fragment.
                // So instead of applying a type condition to the last element of a path
                // We keep track of type conditions and apply them to the parent if applicable.
                // EG:
                // foo.bar|[baz, qux] # |[baz, qux] aren't necessary
                // foo.bar|[baz, qux].quux # |[baz, qux] apply to the parents, they are necessary
                if self.possible_types_after_last_field.len() != self.possible_types.len() {
                    let conditions = &self.possible_types;

                    match new_path.pop() {
                        Some(FetchDataPathElement::AnyIndex(_)) => {
                            new_path.push(FetchDataPathElement::AnyIndex(Some(
                                conditions.iter().cloned().collect(),
                            )));
                        }
                        Some(FetchDataPathElement::Key(name, _)) => {
                            new_path.push(FetchDataPathElement::Key(
                                name,
                                Some(conditions.iter().cloned().collect()),
                            ));
                        }
                        Some(other) => new_path.push(other),
                        // TODO: We should be emitting type conditions here on no element like the
                        //       JS code, which requires a new FetchDataPathElement variant in Rust.
                        //       This really has to do with a hack we did to avoid changing fetch
                        //       data paths too much, in which type conditions ought to be their own
                        //       variant entirely.
                        None => {}
                    }
                }

                new_path.push(FetchDataPathElement::Key(
                    field.response_name().clone(),
                    Default::default(),
                ));

                // TODO: is there a simpler way to find a field’s type from `&Field`?
                let mut type_ = &field.field_position.get(field.schema.schema())?.ty;
                loop {
                    match type_ {
                        Type::Named(_) | Type::NonNullNamed(_) => break,
                        Type::List(inner) | Type::NonNullList(inner) => {
                            new_path.push(FetchDataPathElement::AnyIndex(Default::default()));
                            type_ = inner
                        }
                    }
                }

                Ok(new_path)
            }
        }
    }
}

impl FetchDependencyGraph {
    pub(crate) fn new(
        supergraph_schema: ValidFederationSchema,
        federated_query_graph: Arc<QueryGraph>,
        root_type_for_defer: Option<CompositeTypeDefinitionPosition>,
        fetch_id_generation: Arc<FetchIdGenerator>,
    ) -> Self {
        Self {
            defer_tracking: DeferTracking::empty(&supergraph_schema, root_type_for_defer),
            supergraph_schema,
            federated_query_graph,
            graph: Default::default(),
            root_nodes_by_subgraph: Default::default(),
            fetch_id_generation,
            is_reduced: false,
            is_optimized: false,
            reduced_defer_edges: Vec::new(),
        }
    }

    pub(crate) fn root_node_by_subgraph_iter(
        &self,
    ) -> impl Iterator<Item = (&Arc<str>, &NodeIndex)> {
        self.root_nodes_by_subgraph.iter()
    }

    /// Must be called every time the "shape" of the graph is modified
    /// to know that the graph may not be minimal/optimized anymore.
    fn on_modification(&mut self) {
        self.is_reduced = false;
        self.is_optimized = false;
    }

    pub(crate) fn get_or_create_root_node(
        &mut self,
        subgraph_name: &Arc<str>,
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
            None,
            None,
        )?;
        self.root_nodes_by_subgraph
            .insert(subgraph_name.clone(), node);
        Ok(node)
    }

    fn new_root_type_node(
        &mut self,
        subgraph_name: Arc<str>,
        root_kind: SchemaRootDefinitionKind,
        parent_type: &ObjectTypeDefinitionPosition,
        merge_at: Option<Vec<FetchDataPathElement>>,
        defer_ref: Option<DeferRef>,
    ) -> Result<NodeIndex, FederationError> {
        let has_inputs = false;
        self.new_node(
            subgraph_name,
            parent_type.clone().into(),
            has_inputs,
            root_kind,
            merge_at,
            defer_ref,
        )
    }

    pub(crate) fn new_node(
        &mut self,
        subgraph_name: Arc<str>,
        parent_type: CompositeTypeDefinitionPosition,
        has_inputs: bool,
        root_kind: SchemaRootDefinitionKind,
        merge_at: Option<Vec<FetchDataPathElement>>,
        defer_ref: Option<DeferRef>,
    ) -> Result<NodeIndex, FederationError> {
        let subgraph_schema = self
            .federated_query_graph
            .schema_by_source(&subgraph_name)?
            .clone();
        self.on_modification();
        let merge_key_hash = compute_merge_key_hash(&subgraph_name, &merge_at, &defer_ref);
        Ok(self.graph.add_node(Arc::new(FetchDependencyGraphNode {
            subgraph_name,
            root_kind,
            selection_set: FetchSelectionSet::empty(subgraph_schema, parent_type.clone())?,
            parent_type,
            is_entity_fetch: has_inputs,
            inputs: has_inputs
                .then(|| Arc::new(FetchInputs::empty(self.supergraph_schema.clone()))),
            input_rewrites: Default::default(),
            merge_at,
            merge_key_hash,
            id: OnceLock::new(),
            defer_ref,
            cached_cost: None,
            must_preserve_selection_set: false,
            is_known_useful: false,
            context_inputs: Vec::new(),
        })))
    }

    pub(crate) fn node_weight(
        &self,
        node: NodeIndex,
    ) -> Result<&Arc<FetchDependencyGraphNode>, FederationError> {
        self.graph
            .node_weight(node)
            .ok_or_else(|| FederationError::internal("Node unexpectedly missing"))
    }

    /// Does not take `&mut self` so that other fields can be mutated while this borrow lasts
    fn node_weight_mut(
        graph: &mut FetchDependencyGraphPetgraph,
        node: NodeIndex,
    ) -> Result<&mut FetchDependencyGraphNode, FederationError> {
        Ok(Arc::make_mut(graph.node_weight_mut(node).ok_or_else(
            || FederationError::internal("Node unexpectedly missing".to_owned()),
        )?))
    }

    fn get_or_create_key_node(
        &mut self,
        subgraph_name: &Arc<str>,
        merge_at: &[FetchDataPathElement],
        type_: &CompositeTypeDefinitionPosition,
        parent: ParentRelation,
        conditions_nodes: &IndexSet<NodeIndex>,
        defer_ref: Option<&DeferRef>,
    ) -> Result<NodeIndex, FederationError> {
        // Let's look if we can reuse a node we have, that is an existing child of the parent that:
        // 1. is for the same subgraph
        // 2. has the same merge_at
        // 3. is for the same entity type (we don't reuse nodes for different entities just yet,
        //    as this can create unnecessary dependencies that gets in the way of some optimizations;
        //    the final optimizations in `reduceAndOptimize` will however later merge nodes
        //    on the same subgraph and mergeAt when possible).
        // 4. is not part of our conditions or our conditions ancestors
        //    (meaning that we annot reuse a node if it fetches something we take as input).
        // 5. is part of the same "defer" grouping
        // 6. has the same path in parents (here again, we _will_ eventually merge fetches
        //    for which this is not true later in `reduceAndOptimize`, but for now,
        //    keeping nodes separated when they have a different path in their parent
        //    allows to keep that "path in parent" more precisely,
        //    which is important for some case of @requires).
        for existing_id in
            FetchDependencyGraph::sorted_nodes(self.children_of(parent.parent_node_id))
        {
            let existing = self.node_weight(existing_id)?;
            // we compare the subgraph names last because on average it improves performance
            if existing.merge_at.as_deref() == Some(merge_at)
                && existing
                    .selection_set
                    .selection_set
                    .selections
                    .values()
                    .all(|selection| {
                        matches!(
                            selection,
                            Selection::InlineFragment(fragment)
                            if fragment.casted_type() == type_
                        )
                    })
                && !self.is_in_nodes_or_their_ancestors(existing_id, conditions_nodes)
                && self
                    .parents_relations_of(existing_id)
                    .find(|rel| rel.parent_node_id == parent.parent_node_id)
                    .and_then(|rel| rel.path_in_parent)
                    == parent.path_in_parent
                && existing.defer_ref.as_ref() == defer_ref
                && existing.subgraph_name == *subgraph_name
            {
                return Ok(existing_id);
            }
        }
        let new_node = self.new_key_node(subgraph_name, merge_at.to_vec(), defer_ref.cloned())?;
        self.add_parent(new_node, parent);
        Ok(new_node)
    }

    fn new_key_node(
        &mut self,
        subgraph_name: &Arc<str>,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<DeferRef>,
    ) -> Result<NodeIndex, FederationError> {
        let entity_type = self
            .federated_query_graph
            .schema_by_source(subgraph_name)?
            .entity_type()?
            .ok_or_else(|| {
                FederationError::internal(format!(
                    "Subgraph `{subgraph_name}` has no entities defined"
                ))
            })?;

        self.new_node(
            subgraph_name.clone(),
            entity_type.into(),
            /* has_inputs: */ true,
            SchemaRootDefinitionKind::Query,
            Some(merge_at),
            defer_ref,
        )
    }

    /// Adds another node as a parent of `child`,
    /// meaning that this fetch should happen after the provided one.
    /// Assumption: The parent node is not a descendant of the child.
    fn add_parent(&mut self, child_id: NodeIndex, parent_relation: ParentRelation) {
        let ParentRelation {
            parent_node_id,
            path_in_parent,
        } = parent_relation;
        if self.graph.contains_edge(parent_node_id, child_id) {
            return;
        }
        assert!(
            !self.is_descendant_of(parent_node_id, child_id),
            "Node {parent_node_id:?} is a descendant of {child_id:?}: \
             adding it as parent would create a cycle"
        );
        self.on_modification();
        self.graph.add_edge(
            parent_node_id,
            child_id,
            Arc::new(FetchDependencyGraphEdge {
                path: path_in_parent,
            }),
        );
    }

    fn copy_inputs(
        &mut self,
        node_id: NodeIndex,
        other_id: NodeIndex,
    ) -> Result<(), FederationError> {
        let (node, other_node) = self.graph.index_twice_mut(node_id, other_id);
        Arc::make_mut(node).copy_inputs(other_node)
    }

    /// Returns true if `needle` is either part of `haystack`, or is one of their ancestors
    /// (potentially recursively).
    fn is_in_nodes_or_their_ancestors(
        &self,
        needle: NodeIndex,
        haystack: &IndexSet<NodeIndex>,
    ) -> bool {
        if haystack.contains(&needle) {
            return true;
        }

        // No risk of infinite loop as the graph is acyclic:
        let mut to_check = haystack.clone();
        while let Some(next) = to_check.pop() {
            for parent in self.parents_of(next) {
                if parent == needle {
                    return true;
                }
                to_check.insert(parent);
            }
        }
        false
    }

    fn is_parent_of(&self, node_id: NodeIndex, maybe_child_id: NodeIndex) -> bool {
        self.parents_of(maybe_child_id).any(|id| id == node_id)
    }

    fn is_descendant_of(&self, node_id: NodeIndex, maybe_ancestor_id: NodeIndex) -> bool {
        petgraph::algo::has_path_connecting(&self.graph, maybe_ancestor_id, node_id, None)
    }

    /// Returns whether `node_id` is both a child of `maybe_parent_id` but also if we can show that the
    /// dependency between the nodes is "artificial" in the sense that this node inputs do not truly
    /// depend on anything `maybe_parent` fetches and `maybe_parent` is not a top level selection.
    fn is_child_of_with_artificial_dependency(
        &self,
        node_id: NodeIndex,
        maybe_parent_id: NodeIndex,
    ) -> Result<bool, FederationError> {
        let maybe_parent = self.node_weight(maybe_parent_id)?;
        if maybe_parent.is_top_level() {
            return Ok(false);
        }

        // To be a child with an artificial dependency, it needs to be a child first, and the "path in parent" should be known.
        let Some(relation) = self.parent_relation(node_id, maybe_parent_id) else {
            return Ok(false);
        };
        let Some(path_in_parent) = relation.path_in_parent else {
            return Ok(false);
        };

        let node = self.node_weight(node_id)?;
        // Then, if we have no inputs, we know we don't depend on anything from the parent no matter what.
        let Some(node_inputs) = &node.inputs else {
            return Ok(true);
        };

        // If we do have inputs, then we first look at the path to `maybe_parent` which needs to be
        // "essentially empty". "essentially" is because path can sometimes have some leading fragment(s)
        // and those are fine to ignore. But if the path has some fields, then this implies that the inputs
        // of `node` are based on something at a deeper level than those of `maybe_parent`, and the "contains"
        // comparison we do below would not make sense.
        if path_in_parent
            .0
            .iter()
            .any(|p| matches!(**p, OpPathElement::Field(_)))
        {
            return Ok(false);
        }

        // In theory, the most general test we could have here is to check if `node.inputs` "intersects"
        // `maybe_parent.selection. As if it doesn't, we know our inputs don't depend on anything the
        // parent fetches. However, selection set intersection is a bit tricky to implement (due to fragments,
        // it would be a bit of code to do not-too-inefficiently, but both fragments and alias makes the
        // definition of what the intersection we'd need here fairly subtle), and getting it wrong could
        // make us generate incorrect query plans. Adding to that, the current known cases for this method
        // being useful happens to be when `node.inputs` and `maybe_parent.inputs` are the same. Now, checking
        // inputs is a bit weaker, in the sense that the input could be different and yet the child node
        // not depend on anything the parent fetches, but it is "sufficient", in that if the inputs of the
        // parent includes entirely the child inputs, then we know nothing the child needs can be fetched
        // by the parent (or rather, if it does, it's useless). Anyway, checking inputs inclusion is easier
        // to do so we rely on this for now. If we run into examples where this happens to general non-optimal
        // query plan, we can decide then to optimize further and implement a proper intersections.
        let Some(parent_inputs) = &maybe_parent.inputs else {
            return Ok(false);
        };
        Ok(parent_inputs.contains(node_inputs))
    }

    fn children_of(&self, node_id: NodeIndex) -> impl Iterator<Item = NodeIndex> {
        self.graph
            .neighbors_directed(node_id, petgraph::Direction::Outgoing)
    }

    fn parent_relation(
        &self,
        node_id: NodeIndex,
        maybe_parent_id: NodeIndex,
    ) -> Option<ParentRelation> {
        self.parents_relations_of(node_id)
            .find(|p| p.parent_node_id == maybe_parent_id)
    }

    fn parents_of(&self, node_id: NodeIndex) -> impl Iterator<Item = NodeIndex> {
        self.graph
            .neighbors_directed(node_id, petgraph::Direction::Incoming)
    }

    fn parents_relations_of(&self, node_id: NodeIndex) -> impl Iterator<Item = ParentRelation> {
        self.graph
            .edges_directed(node_id, petgraph::Direction::Incoming)
            .map(|edge| ParentRelation {
                parent_node_id: edge.source(),
                path_in_parent: edge.weight().path.clone(),
            })
    }

    /// By default, petgraph iterates over the nodes in the order of their node indices, but if
    /// we retrieve node iterator based on the edges (e.g. children of/parents of), then resulting
    /// iteration order is unspecified. In practice, it appears that edges are iterated in the
    /// *reverse* iteration order.
    ///
    /// Since this behavior can affect the query plans, we can use this method to explicitly sort
    /// the iterator to ensure we consistently follow the node index order.
    ///
    /// NOTE: In JS implementation, whenever we remove/merge nodes, we always shift left remaining
    /// nodes so there are no gaps in the node IDs and the newly created nodes are always created
    /// with the largest IDs. RS implementation has different behavior - whenever nodes are removed,
    /// their IDs are later reused by petgraph so we no longer have guarantee that node with the
    /// largest ID is the last node that was created. Due to the above, sorting by node IDs may still
    /// result in different iteration order than the JS code, but in practice might be enough to
    /// ensure correct plans.
    fn sorted_nodes(nodes: impl Iterator<Item = NodeIndex>) -> impl Iterator<Item = NodeIndex> {
        nodes.sorted_by_key(|n| n.index())
    }

    fn type_for_fetch_inputs(
        &self,
        type_name: &Name,
    ) -> Result<CompositeTypeDefinitionPosition, FederationError> {
        Ok(self.supergraph_schema.get_type(type_name)?.try_into()?)
    }

    /// Find redundant edges coming out of a node. See `remove_redundant_edges`. This method assumes
    /// that the underlying graph does not have any cycles between nodes.
    ///
    /// PORT NOTE: JS implementation performs in-place removal of edges when finding the redundant
    /// edges. In RS implementation we first collect the edges and then remove them. This has a side
    /// effect that if we ever end up with a cycle in a graph (which is an invalid state), this method
    /// may result in infinite loop.
    fn collect_redundant_edges(&self, node_index: NodeIndex, acc: &mut IndexSet<EdgeIndex>) {
        let mut stack = vec![];
        for start_index in self.children_of(node_index) {
            stack.extend(self.children_of(start_index));
            while let Some(v) = stack.pop() {
                for edge in self.graph.edges_connecting(node_index, v) {
                    acc.insert(edge.id());
                }
                stack.extend(self.children_of(v));
            }
        }
    }

    /// Do a transitive reduction for edges coming out of the given node.
    ///
    /// If any deeply nested child of this node has an edge to any direct child of this node, the
    /// direct child is removed, as we know it is also reachable through the deeply nested route.
    fn remove_redundant_edges(&mut self, node_index: NodeIndex) {
        let mut redundant_edges = IndexSet::default();
        self.collect_redundant_edges(node_index, &mut redundant_edges);

        // Same defer-boundary preservation as `reduce()`: post-merge re-reduction
        // can create newly-redundant edges that cross a defer boundary, and those
        // dependencies must also be restored at extract time.
        self.record_reduced_defer_edges(&redundant_edges);

        if !redundant_edges.is_empty() {
            self.on_modification();
        }
        for edge in redundant_edges {
            self.graph.remove_edge(edge);
        }
    }

    fn remove_node(&mut self, node_index: NodeIndex) {
        self.on_modification();
        // Drop any reduced-defer-edge entries that reference a node we're removing.
        // A removed node (empty/useless) has no data, so the recovered defer dependency
        // on/from it is vacuous. For merges the caller must call `remap_reduced_defer_edges`
        // first so that references are redirected to the surviving node.
        self.reduced_defer_edges
            .retain(|&(s, t)| s != node_index && t != node_index);
        self.graph.remove_node(node_index);
    }

    /// Redirect any `reduced_defer_edges` entries referencing `from` so that they
    /// reference `to` instead. Called when `from` is being merged into `to`: the
    /// merged node's data now lives under `to`, so the recovered defer dependency
    /// must follow.
    fn remap_reduced_defer_edges(&mut self, from: NodeIndex, to: NodeIndex) {
        if from == to {
            return;
        }
        for entry in &mut self.reduced_defer_edges {
            if entry.0 == from {
                entry.0 = to;
            }
            if entry.1 == from {
                entry.1 = to;
            }
        }
    }

    /// Retain nodes that satisfy the given predicate and remove the rest.
    /// - Calls `on_modification` if necessary.
    fn retain_nodes(&mut self, predicate: impl Fn(&NodeIndex) -> bool) {
        // PORT_NOTE: We let `petgraph` to handle the removal of the edges as well, while the JS
        //            version has more code to do that itself.
        let node_count_before = self.graph.node_count();
        self.graph
            .retain_nodes(|_, node_index| predicate(&node_index));
        if self.graph.node_count() < node_count_before {
            // Drop reduced-defer-edge entries that touch a removed node so that
            // `collect_reduced_defer_dependencies` never tries to look one up.
            self.reduced_defer_edges
                .retain(|&(s, t)| predicate(&s) && predicate(&t));
            // PORT_NOTE: There are several different places that call `onModification` in JS. Here we
            //            call it just once, but it should be ok, since the function is idempotent.
            self.on_modification();
        }
    }

    /// - Calls `on_modification` if necessary.
    fn remove_child_edge(&mut self, node_index: NodeIndex, child_index: NodeIndex) {
        if !self.is_parent_of(node_index, child_index) {
            return;
        }

        self.on_modification();
        let edges_to_remove: Vec<EdgeIndex> = self
            .graph
            .edges_connecting(node_index, child_index)
            .map(|edge| edge.id())
            .collect();
        for edge in edges_to_remove {
            self.graph.remove_edge(edge);
        }
    }

    /// Do a transitive reduction (https://en.wikipedia.org/wiki/Transitive_reduction) of the graph
    /// using petgraph's Habib-Morvan-Rampon algorithm.
    ///
    /// `StableDiGraph` does not implement `NodeCompactIndexable`, so we build
    /// the toposorted adjacency list manually using a `Vec` to map stable
    /// `NodeIndex` values to dense toposort ranks.
    fn reduce(&mut self) {
        if self.is_reduced {
            return;
        }

        let Some((adj, tred_index_of)) = to_toposorted_adjacency_list(&self.graph) else {
            return;
        };
        let (tred, _) = dag_transitive_reduction_closure(&adj);

        let edge_in_reduction = |source: NodeIndex, target: NodeIndex| {
            let src_tred_idx = tred_index_of[source.index()];
            let tgt_tred_idx = tred_index_of[target.index()];
            tred.neighbors(petgraph::adj::NodeIndex::new(src_tred_idx))
                .any(|n| n.index() == tgt_tred_idx)
        };

        // Remember defer-crossing edges before they're removed, so we can
        // restore them as defer dependencies later.
        let mut modified = false;
        for edge in self.graph.edge_references() {
            if !edge_in_reduction(edge.source(), edge.target()) {
                if self.graph[edge.source()].defer_ref != self.graph[edge.target()].defer_ref {
                    self.reduced_defer_edges
                        .push((edge.source(), edge.target()));
                }
                modified = true;
            }
        }

        // PORT_NOTE: JS version calls `FetchGroup.removeChild`, which calls onModification.
        if modified {
            self.on_modification();
            self.graph
                .retain_edges(|graph, edge_idx| match graph.edge_endpoints(edge_idx) {
                    Some((source, target)) => edge_in_reduction(source, target),
                    None => false,
                });
        }

        self.is_reduced = true;
    }

    /// Record edges from `redundant_edges` that cross a defer boundary
    /// (`source.defer_ref != target.defer_ref`) into `reduced_defer_edges`,
    /// where `collect_reduced_defer_dependencies` will later restore them.
    fn record_reduced_defer_edges(&mut self, redundant_edges: &IndexSet<EdgeIndex>) {
        for &edge in redundant_edges {
            let Some((source, target)) = self.graph.edge_endpoints(edge) else {
                continue;
            };
            if self.graph[source].defer_ref != self.graph[target].defer_ref {
                self.reduced_defer_edges.push((source, target));
            }
        }
    }

    /// Reduce the graph (see `reduce`) and then do a some additional traversals to optimize for:
    ///  1) fetches with no selection: this can happen when we have a require if the only field requested
    ///     was the one with the require and that forced some dependencies. Those fetch should have
    ///     no dependents and we can just remove them.
    ///  2) fetches that are made in parallel to the same subgraph and the same path, and merge those.
    fn reduce_and_optimize(&mut self) -> Result<(), FederationError> {
        if self.is_optimized {
            return Ok(());
        }

        self.reduce();

        self.remove_empty_nodes();

        self.remove_useless_nodes()?;

        self.merge_child_fetches_for_same_subgraph_and_path()?;

        self.merge_fetches_to_same_subgraph_and_same_inputs()?;

        self.is_optimized = true;
        Ok(())
    }

    fn is_root_node(&self, node_index: NodeIndex, node: &FetchDependencyGraphNode) -> bool {
        self.root_nodes_by_subgraph
            .get(&node.subgraph_name)
            .is_some_and(|root_node_id| *root_node_id == node_index)
    }

    /// - Calls `on_modification` if necessary.
    fn remove_empty_nodes(&mut self) {
        // Note: usually, empty nodes are due to temporary nodes created during the handling of
        // @require and note needed. There is a special case with @defer however whereby everything
        // in a query is deferred (not very useful in practice, but not disallowed by the spec),
        // and in that case we will end up with an empty root node. In that case, we don't remove
        // that node, but instead will recognize that case when processing nodes later.

        let is_removable = |node_index: NodeIndex, node: &FetchDependencyGraphNode| {
            node.selection_set.selection_set.selections.is_empty()
                && !self.is_root_node(node_index, node)
        };
        let to_remove: IndexSet<NodeIndex> = self
            .graph
            .node_references()
            .filter_map(|(node_index, node)| is_removable(node_index, node).then_some(node_index))
            .collect();

        if to_remove.is_empty() {
            return; // unchanged
        }
        // Note: We remove empty nodes without relocating their children. The invariant is that
        // the children of empty nodes (if any) must be accessible from the root via another path.
        // Otherwise, they would've become inaccessible orphan nodes.
        self.retain_nodes(|node_index| !to_remove.contains(node_index));
    }

    /// - Calls `on_modification` if necessary.
    fn remove_useless_nodes(&mut self) -> Result<(), FederationError> {
        let root_nodes: Vec<_> = self.root_node_by_subgraph_iter().map(|(_, i)| *i).collect();
        root_nodes
            .into_iter()
            .try_for_each(|node_index| self.remove_useless_nodes_bottom_up(node_index))
    }

    /// Recursively collect removable useless nodes from the bottom up.
    /// - Calls `on_modification` if necessary.
    fn remove_useless_nodes_bottom_up(
        &mut self,
        node_index: NodeIndex,
    ) -> Result<(), FederationError> {
        // Recursively remove children first, which could make the current node removable.
        for child in self.children_of(node_index).collect::<Vec<_>>() {
            self.remove_useless_nodes_bottom_up(child)?;
        }

        let node = self.node_weight(node_index)?;
        if !self.is_useless_node(node_index, node)? {
            // Record the result of `self.is_useless_node(...)` (if negative).
            let mut_node = Self::node_weight_mut(&mut self.graph, node_index)?;
            mut_node.is_known_useful = true;
            return Ok(()); // not removable
        }

        // In general, removing a node is a bit tricky because we need to deal with
        // the fact that the node can have multiple parents, and we don't have the
        // "path in parent" in all cases. To keep thing relatively easily, we only
        // handle the following cases (other cases will remain non-optimal, but
        // hopefully this handle all the cases we care about in practice):
        //   1. if the node has no children. In which case we can just remove it with
        //      no ceremony.
        //   2. if the node has only a single parent and we have a path to that
        //      parent.

        let has_no_children = {
            let mut children_iter = self.children_of(node_index);
            children_iter.next().is_none()
        };
        if has_no_children {
            self.remove_node(node_index);
            return Ok(());
        }

        let (parent_node_id, path_in_parent) = {
            let mut parents_iter = self.parents_relations_of(node_index);
            let Some(ParentRelation {
                parent_node_id,
                path_in_parent,
            }) = parents_iter.next()
            else {
                // orphan node (can't happen) => ignore (don't bother to remove)
                return Ok(());
            };

            if parents_iter.next().is_some() {
                // More than one parents => not removable
                return Ok(());
            }
            let Some(path_in_parent) = &path_in_parent else {
                // Parent has no path to this node => not removable
                return Ok(());
            };
            (parent_node_id, path_in_parent.clone())
        };
        self.remove_useless_child(parent_node_id, node_index, &path_in_parent);
        Ok(())
    }

    /// `child_path` must be the path in the ParentRelation of `node_id` to `child_id`.
    /// - Calls `on_modification`
    fn remove_useless_child(
        &mut self,
        node_id: NodeIndex,
        child_id: NodeIndex,
        child_path: &Arc<OpPath>,
    ) {
        self.on_modification();
        // Removing the child means attaching all of its children to its parent.
        self.relocate_children_on_merged_in(node_id, child_id, child_path);
        // A "useless" child is one whose fetched fields are already present in its
        // inputs (the parent). Redirect any recorded defer edges from the child to
        // the parent, which actually holds that data.
        self.remap_reduced_defer_edges(child_id, node_id);
        self.remove_node(child_id);
    }

    /// If everything fetched by a node is already part of its inputs, we already have all the data
    /// and there is no need to do the fetch.
    // PORT_NOTE: The JS version memoize the result on the node itself in this function. The Rust version
    // memoize in the `remove_useless_nodes_bottom_up` function.
    fn is_useless_node(
        &self,
        node_index: NodeIndex,
        node: &FetchDependencyGraphNode,
    ) -> Result<bool, FederationError> {
        if node.is_known_useful || node.must_preserve_selection_set {
            return Ok(false);
        }
        let Some(self_inputs) = node.inputs.as_ref() else {
            return Ok(false);
        };

        // Some helper functions

        let try_get_type_condition = |selection: &Selection| match selection {
            Selection::InlineFragment(inline) => {
                inline.inline_fragment.type_condition_position.clone()
            }

            _ => None,
        };

        let get_subgraph_schema = |subgraph_name: &Arc<str>| {
            self.federated_query_graph
                .schema_by_source(subgraph_name)
                .cloned()
        };

        // For nodes that fetches from an @interfaceObject, we can sometimes have something like
        //   { ... on Book { id } } => { ... on Product { id } }
        // where `Book` is an implementation of interface `Product`.
        // And that is because while only "books" are concerned by this fetch, the `Book` type is
        // unknown of the queried subgraph (in that example, it defines `Product` as an
        // @interfaceObject) and so we have to "cast" into `Product` instead of `Book`.
        // But the fetch above _is_ useless, it does only fetch its inputs, and we wouldn't catch
        // this if we do a raw inclusion check of `selection` into `inputs`
        //
        // We only care about this problem at the top-level of the selections however, so we do
        // that top-level check manually (instead of just calling
        // `this.inputs.contains(this.selection)`) but fallback on `contains` for anything deeper.

        let condition_in_supergraph_if_interface_object = |selection: &Selection| {
            let Some(condition) = try_get_type_condition(selection) else {
                return Ok(None);
            };

            if condition.is_object_type() {
                let Ok(condition_in_supergraph) =
                    self.supergraph_schema.get_type(condition.type_name())
                else {
                    // Note that we're checking the true supergraph, not the API schema, so even
                    // @inaccessible types will be found.
                    let condition_name = condition.type_name();
                    return Err(FederationError::internal(format!(
                        "Type {condition_name} should exists in the supergraph"
                    )));
                };
                match condition_in_supergraph {
                    TypeDefinitionPosition::Interface(interface_type) => Ok(Some(interface_type)),
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        };

        // This condition is specific to the case where we're resolving the _concrete_
        // `__typename` field of an interface when coming from an interfaceObject type.
        // i.e. { ... on Product { __typename id }} => { ... on Product { __typename} }
        // This is usually useless at a glance, but in this case we need to actually
        // keep this since this is our only path to resolving the concrete `__typename`.
        let is_interface_type_condition_on_interface_object = |selection: &Selection| {
            let Some(condition) = try_get_type_condition(selection) else {
                return Ok::<_, FederationError>(false);
            };
            if condition.is_interface_type() {
                // Lastly, we just need to check that we're coming from a subgraph
                // that has the type as an interface object in its schema.
                Ok(self
                    .parents_of(node_index)
                    .map(|p| {
                        let p_node = self.node_weight(p)?;
                        let p_subgraph_name = &p_node.subgraph_name;
                        let p_subgraph_schema = get_subgraph_schema(p_subgraph_name)?;
                        let Ok(type_in_parent) = p_subgraph_schema.get_type(condition.type_name())
                        else {
                            return Ok(false);
                        };
                        p_subgraph_schema.is_interface_object_type(type_in_parent)
                    })
                    .process_results(|mut iter| iter.any(|b| b))?)
            } else {
                Ok(false)
            }
        };

        let input_selections: Vec<&Selection> = self_inputs
            .selection_sets_per_parent_type
            .values()
            .flat_map(|s| s.selections.values())
            .collect();
        // Checks that every selection is contained in the input selections.
        node.selection_set
            .selection_set
            .iter()
            .try_fold(true, |acc, selection| {
                // Skip if we encountered a false before.
                // TODO: This `try_fold` is not short-circuiting. We could improve this later.
                if !acc {
                    return Ok(false);
                }

                // If we're coming from an interfaceObject _to_ an interface, we're "resolving" the
                // concrete type of the interface and don't want to treat this as useless.
                if is_interface_type_condition_on_interface_object(selection)? {
                    return Ok(false);
                }

                let condition_in_supergraph =
                    condition_in_supergraph_if_interface_object(selection)?;
                let Some(condition_in_supergraph) = condition_in_supergraph else {
                    // We're not in the @interfaceObject case described above. We just check that
                    // an input selection contains the one we check.
                    return Ok(input_selections
                        .iter()
                        .any(|input| input.contains(selection)));
                };

                let impl_type_names: IndexSet<_> = self
                    .supergraph_schema
                    .possible_runtime_types(condition_in_supergraph.clone().into())?
                    .iter()
                    .map(|t| t.type_name.clone())
                    .collect();
                // Find all the input selections that selects object for this interface, that is
                // selection on either the interface directly or on one of it's implementation type
                // (we keep both kind separate).
                let mut interface_input_selections: Vec<&Selection> = Vec::new();
                let mut implementation_input_selections: Vec<&Selection> = Vec::new();
                for input_selection in input_selections.iter() {
                    let Some(type_condition) = try_get_type_condition(input_selection) else {
                        return Err(FederationError::internal(format!(
                            "Unexpected input selection {input_selection} on {}",
                            node.display(node_index)
                        )));
                    };
                    if *type_condition.type_name() == condition_in_supergraph.type_name {
                        interface_input_selections.push(input_selection);
                    } else if impl_type_names.contains(type_condition.type_name()) {
                        implementation_input_selections.push(input_selection);
                    }
                }

                let Some(sub_selection_set) = selection.selection_set() else {
                    // we're only here if `conditionInSupergraphIfInterfaceObject` returned something,
                    // we imply that selection is a fragment selection and so has a sub-selectionSet.
                    return Err(FederationError::internal(format!(
                        "Expected a sub-selection set on {selection}"
                    )));
                };

                // If there is some selections on the interface, then the selection needs to be
                // contained in those. Otherwise, if there is implementation selections, it must be
                // contained in _each_ of them (we shouldn't have the case where there is neither
                // interface nor implementation selections, but we just return false if that's the
                // case as a "safe" default).
                if !interface_input_selections.is_empty() {
                    Ok(interface_input_selections.iter().any(|input| {
                        let Some(input_selection_set) = input.selection_set() else {
                            return false;
                        };
                        input_selection_set.contains(sub_selection_set)
                    }))
                } else if !implementation_input_selections.is_empty() {
                    Ok(implementation_input_selections.iter().all(|input| {
                        let Some(input_selection_set) = input.selection_set() else {
                            return false;
                        };
                        input_selection_set.contains(sub_selection_set)
                    }))
                } else {
                    Ok(false)
                }
            })
    }

    /// - Calls `on_modification` if necessary.
    fn merge_child_fetches_for_same_subgraph_and_path(&mut self) -> Result<(), FederationError> {
        let root_nodes: Vec<_> = self.root_node_by_subgraph_iter().map(|(_, i)| *i).collect();
        root_nodes.into_iter().try_for_each(|node_index| {
            self.recursive_merge_child_fetches_for_same_subgraph_and_path(node_index)
        })
    }

    /// Recursively merge child fetches top-down
    /// - Calls `on_modification` if necessary.
    fn recursive_merge_child_fetches_for_same_subgraph_and_path(
        &mut self,
        node_index: NodeIndex,
    ) -> Result<(), FederationError> {
        // We're traversing the `self.graph` in DFS order and mutate it top-down.
        // - Assuming the graph is a DAG and has no cycle.
        let children_nodes: Vec<_> = self.children_of(node_index).collect();
        if children_nodes.len() > 1 {
            // We iterate on all pairs of children and merge those siblings that can be merged
            // together.
            // We will have two indices `i` and `j` such that `i < j`. When we merge `i` and `j`,
            // `i`-th node will be merged into `j`-th node and skip the rest of `j` iteration,
            // since `i` is dead and we are no longer looking for another node to merge `i` into.
            //
            // PORT_NOTE: The JS version merges `j` into `i` instead of `i` into `j`, relying on
            // the `merge_sibling_in` would shrink `children_nodes` dynamically. I found it easier
            // to reason about it the other way around by incrementing `i` when it's merged into
            // `j` without modifying `children_nodes`.
            for (i, i_node_index) in children_nodes.iter().copied().enumerate() {
                for (_j, j_node_index) in children_nodes.iter().copied().enumerate().skip(i + 1) {
                    if self.can_merge_sibling_in(j_node_index, i_node_index)? {
                        // Merge node `i` into node `j`.
                        // In theory, we can merge in any direction. But, we merge i into j,
                        // so `j` can be visited again in the outer loop.
                        self.merge_sibling_in(j_node_index, i_node_index)?;

                        // We're working on a minimal graph (we've done a transitive reduction
                        // beforehand) and we need to keep the graph minimal as post-reduce steps
                        // (the `process` method) rely on it. But merging 2 nodes _can_ break
                        // minimality.
                        // Say we have:
                        //   0 ------
                        //            \
                        //             4
                        //   1 -- 3 --/
                        // and we merge nodes 0 and 1 (and let's call the result "2"), then we now
                        // have:
                        //      ------
                        //     /       \
                        //   2 <-- 3 -- 4
                        // which is not minimal.
                        //
                        // So to fix it, we just re-run our dfs removal from that merged edge
                        // (which is probably a tad overkill in theory, but for the reasons
                        // mentioned on `reduce`, this is most likely a non-issue in practice).
                        //
                        // Note that this DFS can only affect the descendants of `j` (its children
                        // and recursively so), so it does not affect our current iteration.
                        self.remove_redundant_edges(j_node_index);

                        break; // skip the rest of `j`'s iteration
                    }
                }
            }
        }

        // Now we recurse to the sub-nodes.
        // Note: `children_nodes` above may contain invalid nodes at this point.
        //       So, we need to re-collect the children nodes after the merge.
        let children_nodes_after_merge: Vec<_> = self.children_of(node_index).collect();
        children_nodes_after_merge
            .into_iter()
            .try_for_each(|c| self.recursive_merge_child_fetches_for_same_subgraph_and_path(c))
    }

    fn merge_fetches_to_same_subgraph_and_same_inputs(&mut self) -> Result<(), FederationError> {
        // Sometimes, the query will directly query some fields that are also requirements for some
        // other queried fields, and because there is complex dependencies involved, we won't be
        // able to easily realize that we're doing the same fetch to a subgraph twice in 2
        // different places (once for the user query, once for the require). For an example of this
        // happening, see the test called 'handles diamond-shaped dependencies' in
        // `buildPlan.test.ts` Of course, doing so is always inefficient and so this method ensures
        // we merge such fetches.
        // In practice, this method merges any 2 fetches that are to the same subgraph and same
        // mergeAt, and have the exact same inputs.

        // To find which nodes are to the same subgraph and mergeAt somewhat efficiently, we
        // generate a simple string key from each node subgraph name and mergeAt. We do "sanitize"
        // subgraph name, but have no worries for `mergeAt` since it contains either number of
        // field names, and the later is restricted by graphQL so as to not be an issue.
        // PORT_NOTE: The JS version iterates over the nodes in their index order, which is also
        // the insertion order. The Rust version uses a topological sort to ensure that we never
        // merge an ancestor node into a descendant node. JS version's insertion order is almost
        // topologically sorted, thanks to the way the graph is constructed from the root. However,
        // it's not exactly topologically sorted. So, it's unclear whether that is 100% safe.
        // Note: `by_subgraphs` must be an insertion-ordered map. A HashMap-backed map here
        // iterates its keys in a `RandomState`-random order per planning call, which executes
        // the per-key merges below in a random across-key order; the merge order changes edge
        // insertion order and merge outcomes downstream, producing nondeterministic plan shapes
        // (differing `_entities` type-condition batching) across identical calls.
        // `MultiIndexMap` preserves insertion order for both keys and the values of each key:
        // keys are inserted in topological-sort order, and values pushed under the same key
        // inherit that order too, so each key's values remain topologically sorted as well.
        let mut by_subgraphs = MultiIndexMap::new();
        let sorted_nodes = petgraph::algo::toposort(&self.graph, None)
            .map_err(|_| FederationError::internal("Failed to sort nodes due to cycle(s)"))?;
        for node_index in sorted_nodes {
            let node = self.node_weight(node_index)?;
            // We exclude nodes without inputs because that's what we look for. In practice, this
            // mostly just exclude root nodes, which we don't really want to bother with anyway.
            let Some(key) = node.subgraph_and_merge_at_key() else {
                continue;
            };
            by_subgraphs.insert(key, node_index);
        }

        for (_key, nodes) in by_subgraphs {
            // In most cases `nodes` is going be a single element, so skip the trivial case.
            if nodes.len() < 2 {
                continue;
            }

            // Create disjoint sets of the nodes.
            // buckets: an array where each entry is a "bucket" of nodes that can all be merge together.
            let mut buckets: Vec<(NodeIndex, Vec<NodeIndex>)> = Vec::new();
            let has_equal_inputs = |a: NodeIndex, b: NodeIndex| {
                let a_node = self.node_weight(a)?;
                let b_node = self.node_weight(b)?;
                if a_node.defer_ref != b_node.defer_ref {
                    return Ok::<_, FederationError>(false);
                }
                match (&a_node.inputs, &b_node.inputs) {
                    (Some(a), Some(b)) => Ok(a.equals(b)),
                    (None, None) => Ok(true),
                    _ => Ok(false),
                }
            };
            'outer: for node in nodes {
                // see if there is an existing bucket for this node
                for (bucket_head, bucket) in &mut buckets {
                    if has_equal_inputs(*bucket_head, node)? {
                        bucket.push(node);
                        continue 'outer;
                    }
                }
                // No existing bucket found, create a new one.
                buckets.push((node, vec![node]));
            }

            // Merge items in each bucket
            for (_, bucket) in buckets {
                let Some((head, rest)) = bucket.split_first() else {
                    // There is only merging to be done if there is at least one more.
                    continue;
                };

                // We pick the head for the nodes and merge all others into it. Note that which
                // node we choose shouldn't matter since the merging preserves all the
                // dependencies of each group (both parents and children).
                // However, we must not merge an ancestor node into a descendant node. Thus,
                // we choose the head as the first node in the bucket that is also the earliest
                // in the topo-sorted order.
                for node in rest {
                    self.merge_in_with_all_dependencies(*head, *node)?;
                }
            }
        }
        // We may have merged nodes and broke the graph minimality in doing so, so we re-reduce to
        // make sure. Note that if we did no modification to the graph, calling `reduce` is cheap
        // (the `is_reduced` variable will still be `true`).
        self.reduce();
        Ok(()) // done
    }

    fn extract_children_and_deferred_dependencies(
        &mut self,
        node_index: NodeIndex,
    ) -> Result<(Vec<NodeIndex>, DeferredNodes), FederationError> {
        let mut children = vec![];
        let mut deferred_nodes = DeferredNodes::new();

        let mut defer_dependencies = vec![];

        let node_children = self
            .graph
            .neighbors_directed(node_index, petgraph::Direction::Outgoing);
        let node = self.node_weight(node_index)?;
        for child_index in node_children {
            let child = self.node_weight(child_index)?;
            if node.defer_ref == child.defer_ref {
                children.push(child_index);
            } else {
                let Some(child_defer_ref) = &child.defer_ref else {
                    bail!(
                        "{} has defer_ref `{}`, so its child {} cannot have a top-level defer_ref.",
                        node.display(node_index),
                        DisplayOption(node.defer_ref.as_ref()),
                        child.display(child_index),
                    );
                };

                if !node.selection_set.selection_set.selections.is_empty() {
                    let id = *node.id.get_or_init(|| self.fetch_id_generation.next_id());
                    defer_dependencies.push((child_defer_ref.clone(), format!("{id}")));
                }
                deferred_nodes.insert(child_defer_ref.clone(), child_index);
            }
        }

        self.collect_reduced_defer_dependencies(node_index, &mut defer_dependencies)?;

        for (defer_ref, dependency) in defer_dependencies {
            self.defer_tracking.add_dependency(&defer_ref, dependency);
        }

        Ok((children, deferred_nodes))
    }

    /// Collect defer dependencies from edges that were removed during transitive reduction
    /// but crossed a defer boundary.
    ///
    /// Without this, an ancestor fetch whose direct edge to a deferred node was reduced
    /// away (because a transitive path exists through an intermediate fetch) would not be
    /// registered as a dependency, causing the deferred node to miss data at runtime.
    ///
    /// At runtime, each registered dependency broadcasts only its own fetch result to the
    /// deferred node. If an ancestor's edge is reduced away, its result is never broadcast,
    /// and the deferred node loses access to fields only that ancestor provides (e.g.
    /// `__typename`, entity keys, or other required fields at the ancestor's response path).
    ///
    /// To avoid adding unnecessary dependencies, we only restore the edge when the source
    /// node's selection has a field-name intersection with the deferred target's required
    /// inputs. If the source doesn't provide any fields the target needs, the dependency
    /// is purely transitive and can remain reduced.
    fn collect_reduced_defer_dependencies(
        &self,
        node_index: NodeIndex,
        defer_dependencies: &mut Vec<(DeferRef, String)>,
    ) -> Result<(), FederationError> {
        let node = self.node_weight(node_index)?;
        if node.selection_set.selection_set.selections.is_empty() {
            return Ok(());
        }
        for &(source, target) in &self.reduced_defer_edges {
            if source != node_index {
                continue;
            }
            // The target may have been removed between recording and now (e.g.
            // emptied by `remove_empty_nodes` and dropped by `retain_nodes`).
            let Some(child) = self.graph.node_weight(target) else {
                continue;
            };
            if node.defer_ref == child.defer_ref {
                continue;
            }
            let Some(child_defer_ref) = &child.defer_ref else {
                continue;
            };

            // Check if the source's selection provides any fields that the deferred
            // target's inputs require (excluding __typename which is ubiquitous).
            let Some(inputs) = &child.inputs else {
                continue;
            };

            // Navigate the source's selection down to the target's merge_at level,
            // so we compare fields at the same nesting depth. When both nodes have
            // merge_at paths, skip the common prefix and navigate only the remaining
            // suffix (e.g., child merge_at=["start","q"] and node merge_at=["start"]
            // means we navigate the source by just ["q"]).
            let source_selection = match (&child.merge_at, &node.merge_at) {
                (Some(child_path), None) => {
                    match Self::selection_set_at_path(&node.selection_set.selection_set, child_path)
                    {
                        Some(ss) => ss,
                        None => continue,
                    }
                }
                (Some(child_path), Some(node_path)) => {
                    let common_len = child_path
                        .iter()
                        .zip(node_path.iter())
                        .take_while(|(a, b)| a == b)
                        .count();
                    let suffix = &child_path[common_len..];
                    if suffix.is_empty() {
                        &node.selection_set.selection_set
                    } else {
                        match Self::selection_set_at_path(&node.selection_set.selection_set, suffix)
                        {
                            Some(ss) => ss,
                            None => continue,
                        }
                    }
                }
                _ => &node.selection_set.selection_set,
            };

            let has_intersection = inputs
                .selection_sets_per_parent_type
                .values()
                .any(|input_ss| Self::has_field_intersection(source_selection, input_ss));

            if has_intersection {
                let id = *node.id.get_or_init(|| self.fetch_id_generation.next_id());
                defer_dependencies.push((child_defer_ref.clone(), format!("{id}")));
            }
        }
        Ok(())
    }

    /// Navigate a selection set down a `merge_at` path, returning the sub-selection
    /// at that path, or `None` if the path doesn't exist in the selection.
    /// Note: This is an over-approximation, since it does not check if fragment's type condition
    ///       is satisfied by the path's type conditions.
    fn selection_set_at_path<'a>(
        selection_set: &'a SelectionSet,
        path: &[FetchDataPathElement],
    ) -> Option<&'a SelectionSet> {
        let mut current = selection_set;
        for element in path {
            match element {
                FetchDataPathElement::Key(name, _) => {
                    current = Self::find_field_in_selection_set(current, name)?;
                }
                FetchDataPathElement::AnyIndex(_) => {
                    // Array indices don't change the selection structure.
                    continue;
                }
                _ => return None,
            }
        }
        Some(current)
    }

    /// Find a field by response name in a selection set, searching through any layers of
    /// nested inline fragments. Returns the field's sub-selection set.
    fn find_field_in_selection_set<'a>(
        selection_set: &'a SelectionSet,
        name: &Name,
    ) -> Option<&'a SelectionSet> {
        for sel in selection_set.iter() {
            match sel {
                Selection::Field(fs) if fs.field.response_name() == name => {
                    return fs.selection_set.as_ref();
                }
                Selection::InlineFragment(inf) => {
                    if let Some(ss) = Self::find_field_in_selection_set(&inf.selection_set, name) {
                        return Some(ss);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Check whether two selection sets share any field selections (excluding `__typename`).
    /// When an inline fragment (type condition) is found in `b`, it is matched against
    /// the same type condition in `a` via `SelectionMap::get()`, so that fields are only
    /// compared under the same type context.
    /// NOTE: This intersection check is not meant be complete. It's meant to identify matching
    ///       defer node dependencies. We are assuming that the selection sets `a` and `b` have
    ///       a similar structure in the first place.
    fn has_field_intersection(a: &SelectionSet, b: &SelectionSet) -> bool {
        for sel in b.iter() {
            match sel {
                Selection::Field(fs) => {
                    if *fs.field.name() == TYPENAME_FIELD {
                        continue;
                    }
                    if a.selections.get(sel.key()).is_some() {
                        return true;
                    }
                    // Here, `a` may have inline fragments without a type condition that contain
                    // `fs` theoretically. But, our query plan won't generate such inline fragments
                    // unnecessarily.
                }
                Selection::InlineFragment(b_inf) => {
                    // Try to find a matching type condition in `a` first.
                    if let Some(Selection::InlineFragment(a_inf)) = a.selections.get(sel.key())
                        && Self::has_field_intersection(&a_inf.selection_set, &b_inf.selection_set)
                    {
                        return true;
                    }

                    if b_inf.inline_fragment.casted_type() == a.type_position {
                        // No matching inline fragment in `a`, but `a` is already
                        // at the same type as `b`'s type condition. Compare directly.
                        if Self::has_field_intersection(a, &b_inf.selection_set) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn create_state_for_children_of_processed_node(
        &self,
        processed_index: NodeIndex,
        children: impl IntoIterator<Item = NodeIndex>,
    ) -> ProcessingState {
        let mut next = vec![];
        let mut unhandled = vec![];
        for c in children {
            let num_parents = self.parents_of(c).count();
            if num_parents == 1 {
                // The parent we have processed is the only one parent of that child; we can handle the children
                next.push(c)
            } else {
                let parents = self
                    .parents_relations_of(c)
                    .filter(|parent| parent.parent_node_id != processed_index)
                    .collect();
                unhandled.push(UnhandledNode {
                    node: c,
                    unhandled_parents: parents,
                });
            }
        }
        ProcessingState { next, unhandled }
    }

    fn process_node<TProcessed, TDeferred>(
        &mut self,
        processor: &mut impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        node_index: NodeIndex,
        handled_conditions: Conditions,
    ) -> Result<(TProcessed, DeferredNodes, ProcessingState), FederationError> {
        let (children, deferred_nodes) =
            self.extract_children_and_deferred_dependencies(node_index)?;

        let node = self
            .graph
            .node_weight_mut(node_index)
            .ok_or_else(|| FederationError::internal("Node unexpectedly missing"))?;
        let conditions = node
            .selection_set
            .conditions()?
            .update_with(&handled_conditions);
        let new_handled_conditions = conditions.clone().merge(handled_conditions);

        let processed = processor.on_node(
            &self.federated_query_graph,
            Arc::make_mut(node),
            &new_handled_conditions,
        )?;
        if children.is_empty() {
            return Ok((
                processor.on_conditions(&conditions, processed),
                deferred_nodes,
                ProcessingState::empty(),
            ));
        }

        let state = self.create_state_for_children_of_processed_node(node_index, children);
        if state.next.is_empty() {
            Ok((
                processor.on_conditions(&conditions, processed),
                deferred_nodes,
                state,
            ))
        } else {
            // We process the ready children as if they were parallel roots (they are from `processed`
            // in a way), and then just add process at the beginning of the sequence.
            let (main_sequence, all_deferred_nodes, new_state) = self.process_root_main_nodes(
                processor,
                state,
                true,
                &deferred_nodes,
                new_handled_conditions,
            )?;

            let reduced_sequence =
                processor.reduce_sequence(iter::once(processed).chain(main_sequence));
            Ok((
                processor.on_conditions(&conditions, reduced_sequence),
                all_deferred_nodes,
                new_state,
            ))
        }
    }

    fn process_nodes<TProcessed, TDeferred>(
        &mut self,
        processor: &mut impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        state: ProcessingState,
        process_in_parallel: bool,
        handled_conditions: Conditions,
    ) -> Result<(TProcessed, DeferredNodes, ProcessingState), FederationError> {
        let mut processed_nodes = vec![];
        let mut all_deferred_nodes = DeferredNodes::new();
        let mut new_state = ProcessingState {
            next: Default::default(),
            unhandled: state.unhandled,
        };
        for node_index in &state.next {
            let (main, deferred_nodes, state_after_node) =
                self.process_node(processor, *node_index, handled_conditions.clone())?;
            processed_nodes.push(main);
            all_deferred_nodes.extend(deferred_nodes.into_iter());
            new_state = new_state.merge_with(state_after_node);
        }

        // Note that `new_state` is the merged result of everything after each individual node (anything that was _only_ depending
        // on it), but the fact that nodes themselves (`state.next`) have been handled has not necessarily be taking into
        // account yet, so we do it below. Also note that this must be done outside of the `for` loop above, because any
        // node that dependend on multiple of the input nodes of this function must not be handled _within_ this function
        // but rather after it, and this is what ensures it.
        let processed = if process_in_parallel {
            processor.reduce_parallel(processed_nodes)
        } else {
            processor.reduce_sequence(processed_nodes)
        };
        Ok((
            processed,
            all_deferred_nodes,
            new_state.update_for_processed_nodes(&state.next),
        ))
    }

    /// Process the "main" (non-deferred) nodes starting at the provided roots. The deferred nodes are collected
    /// by this method but not otherwise processed.
    fn process_root_main_nodes<TProcessed, TDeferred>(
        &mut self,
        processor: &mut impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        mut state: ProcessingState,
        roots_are_parallel: bool,
        initial_deferred_nodes: &DeferredNodes,
        handled_conditions: Conditions,
    ) -> Result<(Vec<TProcessed>, DeferredNodes, ProcessingState), FederationError> {
        let mut main_sequence = vec![];
        let mut all_deferred_nodes = initial_deferred_nodes.clone();
        let mut process_in_parallel = roots_are_parallel;
        while !state.next.is_empty() {
            let (processed, deferred_nodes, new_state) = self.process_nodes(
                processor,
                state,
                process_in_parallel,
                handled_conditions.clone(),
            )?;
            // After the root nodes, handled on the first iteration, we can process everything in parallel.
            process_in_parallel = true;
            main_sequence.push(processed);
            state = new_state;
            all_deferred_nodes.extend(deferred_nodes.into_iter());
        }

        Ok((main_sequence, all_deferred_nodes, state))
    }

    fn process_root_nodes<TProcessed, TDeferred>(
        &mut self,
        processor: &mut impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        root_nodes: Vec<NodeIndex>,
        roots_are_parallel: bool,
        current_defer_ref: Option<&str>,
        other_defer_nodes: Option<&DeferredNodes>,
        handled_conditions: Conditions,
    ) -> Result<(Vec<TProcessed>, Vec<TDeferred>), FederationError> {
        let (main_sequence, deferred_nodes, new_state) = self.process_root_main_nodes(
            processor,
            ProcessingState::of_ready_nodes(root_nodes),
            roots_are_parallel,
            &Default::default(),
            handled_conditions.clone(),
        )?;
        assert!(
            new_state.next.is_empty(),
            "Should not have left some ready nodes, but got {:?}",
            new_state.next
        );
        assert!(
            new_state.unhandled.is_empty(),
            "Root nodes should have no remaining nodes unhandled, but got: [{}]",
            new_state
                .unhandled
                .iter()
                .map(|unhandled| unhandled.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        let mut all_deferred_nodes = other_defer_nodes.cloned().unwrap_or_default();
        all_deferred_nodes.extend(deferred_nodes.into_iter());

        // We're going to handle all `@defer`s at our "current" level (eg. at the top level, that's all the non-nested @defer),
        // and the "starting" node for those defers, if any, are in `all_deferred_nodes`. However, `all_deferred_nodes`
        // can actually contain defer nodes that are for "deeper" levels of @defer-nesting, and that is because
        // sometimes the key we need to resume a nested @defer is the same as for the current @defer (or put another way,
        // a @defer B may be nested inside @defer A "in the query", but be such that we don't need anything fetched within
        // the deferred part of A to start the deferred part of B).
        // Long story short, we first collect the nodes from `all_deferred_nodes` that are _not_ in our current level, if
        // any, and pass those to the recursive call below so they can be use a their proper level of nesting.
        let defers_in_current = self.defer_tracking.defers_in_parent(current_defer_ref);
        let handled_defers_in_current = defers_in_current
            .iter()
            .map(|info| info.label.clone())
            .collect::<IndexSet<_>>();
        let unhandled_defer_nodes = all_deferred_nodes
            .iter()
            .filter(|(label, _index)| !handled_defers_in_current.contains(*label))
            .map(|(label, index)| (label.clone(), index))
            .collect::<DeferredNodes>();
        let unhandled_defer_node = if unhandled_defer_nodes.is_empty() {
            None
        } else {
            Some(unhandled_defer_nodes)
        };

        // We now iterate on every @defer of said "current level". Note in particular that we may not be able to truly defer
        // anything for some of those @defer due the limitations of what can be done at the query planner level. However, we
        // still create `DeferNode` and `DeferredNode` in those case so that the execution can at least defer the sending of
        // the response back (future handling of defer-passthrough will also piggy-back on this).
        let mut all_deferred: Vec<TDeferred> = vec![];
        // TODO(@goto-bus-stop): this clone looks expensive and could be avoided with a refactor
        // See also PORT_NOTE in `.defers_in_parent()`.
        let defers_in_current = defers_in_current.into_iter().cloned().collect::<Vec<_>>();
        for defer in defers_in_current {
            let nodes = all_deferred_nodes
                .get_all(&defer.label)
                .map_or_else(Default::default, |indices| {
                    indices.iter().copied().collect()
                });
            let (main_sequence_of_defer, deferred_of_defer) = self.process_root_nodes(
                processor,
                nodes,
                true,
                Some(&defer.label),
                unhandled_defer_node.as_ref(),
                handled_conditions.clone(),
            )?;
            let main_reduced = processor.reduce_sequence(main_sequence_of_defer);
            let processed = if deferred_of_defer.is_empty() {
                main_reduced
            } else {
                processor.reduce_defer(main_reduced, &defer.sub_selection, deferred_of_defer)?
            };
            all_deferred.push(processor.reduce_deferred(&defer, processed)?);
        }
        Ok((main_sequence, all_deferred))
    }

    /// Processes the "plan" represented by this query graph using the provided `processor`.
    ///
    /// Returns a main part and a (potentially empty) deferred part.
    pub(crate) fn process<TProcessed, TDeferred>(
        &mut self,
        mut processor: impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        root_kind: SchemaRootDefinitionKind,
    ) -> Result<(TProcessed, Vec<TDeferred>), FederationError> {
        self.reduce_and_optimize()?;

        let (main_sequence, deferred) = self.process_root_nodes(
            &mut processor,
            self.root_nodes_by_subgraph.values().cloned().collect(),
            root_kind == SchemaRootDefinitionKind::Query,
            None,
            None,
            Conditions::Boolean(true),
        )?;

        // Note that the return of `process_root_nodes` should always be reduced as a sequence, regardless of `root_kind`.
        // For queries, it just happens in that the majority of cases, `main_sequence` will be an array of a single element
        // and that single element will be a parallel node of the actual roots. But there is some special cases where some
        // while the roots are started in parallel, the overall plan shape is something like:
        //   Root1 \
        //          -> Other
        //   Root2 /
        // And so it is a sequence, even if the roots will be queried in parallel.
        Ok((processor.reduce_sequence(main_sequence), deferred))
    }

    fn can_merge_child_in(
        &self,
        node_id: NodeIndex,
        child_id: NodeIndex,
    ) -> Result<bool, FederationError> {
        let node = self.node_weight(node_id)?;
        let child = self.node_weight(child_id)?;
        let parent_relation = self.parent_relation(child_id, node_id);

        // we compare the subgraph names last because on average it improves performance
        if !(parent_relation.is_some_and(|r| r.path_in_parent.is_some())
            && node.defer_ref == child.defer_ref
            && node.subgraph_name == child.subgraph_name)
        {
            return Ok(false);
        }

        // `child` may have other parents besides `node_id` -- for instance, a node created to
        // satisfy `child`'s own `@requires`. Merging `child` into `node_id` collapses `child`'s
        // selection into `node_id`'s, which silently drops the dependency edge from any other
        // parent unless `node_id` already (transitively) depends on it. Without this check, an
        // unsatisfied `@requires` on `child` can be merged away entirely (RH-1396).
        //
        // Note this is conservative: some of those other parents may just be fetching `@key`
        // fields (potentially a compound `@key`) for the very subgraph jump this merge would
        // remove, in which case merging would still be safe and this rejects an optimization we
        // could otherwise make. There's no cheap way to distinguish that case from a genuine
        // unsatisfied dependency today, and it's a niche case -- plan correctness matters more
        // than plan optimality here, so we accept the missed optimization.
        for other_parent_id in self.parents_of(child_id) {
            if other_parent_id != node_id && !self.is_descendant_of(node_id, other_parent_id) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// We only allow merging sibling on the same subgraph, same "merge_at" and when the common parent is their only parent:
    /// - there is no reason merging siblings of different subgraphs could ever make sense.
    /// - same "merge_at" paths ensures that we can merge the inputs and selections without having to worry about those
    ///   not being at the same level (hence the empty path in the call to `merge_in_internal` below). In theory, we could
    ///   relax this when we have the "path in parent" for both sibling, and if `sibling_to_merge` is "deeper" than `this`,
    ///   we could still merge it in using the appropriate path. We don't use this yet, but if this get in the way of
    ///   some query plan optimisation, we may have to do so.
    /// - only handling a single parent could be expanded on later, but we don't need it yet so we focus on the simpler case.
    fn can_merge_sibling_in(
        &self,
        node_id: NodeIndex,
        sibling_id: NodeIndex,
    ) -> Result<bool, FederationError> {
        let node = self.node_weight(node_id)?;
        let sibling = self.node_weight(sibling_id)?;

        let own_parents_iter = self
            .graph
            .edges_directed(node_id, petgraph::Direction::Incoming);
        let Some(own_parent_id) = iter_into_single_item(own_parents_iter).map(|node| node.source())
        else {
            return Ok(false);
        };

        let sibling_parents_iter = self
            .graph
            .edges_directed(sibling_id, petgraph::Direction::Incoming);
        let Some(sibling_parent_id) =
            iter_into_single_item(sibling_parents_iter).map(|node| node.source())
        else {
            return Ok(false);
        };

        // Hash check rejects mismatched merge keys without deep Vec comparison.
        // We compare the subgraph names last because on average it improves performance.
        Ok(node.merge_key_hash == sibling.merge_key_hash
            && node.merge_at == sibling.merge_at
            && own_parent_id == sibling_parent_id
            && node.defer_ref == sibling.defer_ref
            && node.subgraph_name == sibling.subgraph_name)
    }

    fn can_merge_grand_child_in(
        &self,
        node_id: NodeIndex,
        child_id: NodeIndex,
        maybe_grand_child_id: NodeIndex,
    ) -> Result<bool, FederationError> {
        let Some(grand_child_parent_relation) =
            iter_into_single_item(self.parents_relations_of(maybe_grand_child_id))
        else {
            return Ok(false);
        };
        let Some(grand_child_parent_parent_relation) =
            self.parent_relation(grand_child_parent_relation.parent_node_id, node_id)
        else {
            return Ok(false);
        };

        let node = self.node_weight(node_id)?;
        let child = self.node_weight(child_id)?;
        let grand_child = self.node_weight(maybe_grand_child_id)?;

        let (Some(child_inputs), Some(grand_child_inputs)) = (&child.inputs, &grand_child.inputs)
        else {
            return Ok(false);
        };

        // we compare the subgraph names last because on average it improves performance
        Ok(grand_child_parent_relation.path_in_parent.is_some()
            && grand_child_parent_parent_relation.path_in_parent.is_some()
            && child.merge_at == grand_child.merge_at
            && child_inputs.contains(grand_child_inputs)
            && node.defer_ref == grand_child.defer_ref
            && node.subgraph_name == grand_child.subgraph_name)
    }

    /// Merges a child of parent node into it.
    ///
    /// Note that it is up to the caller to know that doing such merging is reasonable in the first place, which
    /// generally means knowing that 1) `child.inputs` are included in `parent.inputs` and 2) all of `child.selection`
    /// can safely be queried on the `parent.subgraphName` subgraph.
    ///
    /// Arguments:
    /// * parent_id - parent node ID
    /// * child_id - a node that must be a `child` of this parent, and for which the 'path in parent' (for given parent) is
    ///   known. The `can_merge_child_in` method can be used to ensure that `child` meets those requirement.
    fn merge_child_in(
        &mut self,
        node_id: NodeIndex,
        child_id: NodeIndex,
    ) -> Result<(), FederationError> {
        let Some(relation_to_child) = self.parent_relation(child_id, node_id) else {
            return Err(FederationError::internal(format!(
                "Cannot merge {} into {}: the former is not a child of the latter",
                child_id.index(),
                node_id.index()
            )));
        };
        let Some(child_path_in_this) = relation_to_child.path_in_parent else {
            return Err(FederationError::internal(format!(
                "Cannot merge {} into {}: the path of the former into the latter is unknown",
                child_id.index(),
                node_id.index()
            )));
        };
        self.merge_in_internal(node_id, child_id, &child_path_in_this, false)
    }

    /// Merges a grand child of `this` group into it.
    ///
    /// Note that it is up to the caller to know that doing such merging is reasonable in the first place, which
    /// generally means knowing that 1) `grandChild.inputs` are included in `this.inputs` and 2) all of `grandChild.selection`
    /// can safely be queried on the `this.subgraphName` subgraph (the later of which is trivially true if `this` and
    /// `grandChild` are on the same subgraph and same mergeAt).
    ///
    /// @param grandChild - a group that must be a "grand child" (a child of a child) of `this`, and for which the
    ///   'path in parent' is know for _both_ the grand child to tis parent and that parent to `this`. The `canMergeGrandChildIn`
    ///     method can be used to ensure that `grandChild` meets those requirement.
    fn merge_grand_child_in(
        &mut self,
        node_id: NodeIndex,
        grand_child_id: NodeIndex,
    ) -> Result<(), FederationError> {
        let grand_child_parents: Vec<ParentRelation> =
            self.parents_relations_of(grand_child_id).collect();
        if grand_child_parents.len() != 1 {
            return Err(FederationError::internal(format!(
                "Cannot merge {} as it has multiple parents []",
                grand_child_id.index()
            )));
        }
        let Some(grand_child_grand_parent) =
            self.parent_relation(grand_child_parents[0].parent_node_id, node_id)
        else {
            // assert(gcGrandParent, () => `Cannot merge ${grandChild} into ${this}: the former parent (${gcParent.group}) is not a child of the latter`);
            return Err(FederationError::internal(format!(
                "Cannot merge {} into {}: the former parent {} is not a child of the latter",
                grand_child_id.index(),
                node_id.index(),
                grand_child_parents[0].parent_node_id.index()
            )));
        };
        let (Some(grand_child_parent_path), Some(grand_child_grand_parent_path)) = (
            grand_child_parents[0].path_in_parent.clone(),
            grand_child_grand_parent.path_in_parent,
        ) else {
            // assert(gcParent.path && gcGrandParent.path, () => `Cannot merge ${grandChild} into ${this}: some paths in parents are unknown`);
            return Err(FederationError::internal(format!(
                "Cannot merge {} into {}: some paths in parents are unknown",
                grand_child_id.index(),
                node_id.index()
            )));
        };

        let concatenated_path =
            concat_op_paths(&grand_child_grand_parent_path, &grand_child_parent_path);
        self.merge_in_internal(node_id, grand_child_id, &concatenated_path, false)
    }

    fn merge_sibling_in(
        &mut self,
        node_id: NodeIndex,
        sibling_id: NodeIndex,
    ) -> Result<(), FederationError> {
        let (node, sibling) = self.graph.index_twice_mut(node_id, sibling_id);
        let mutable_node = Arc::make_mut(node);

        mutable_node.copy_inputs(sibling)?;
        self.merge_in_internal(node_id, sibling_id, &OpPath::default(), false)?;

        Ok(())
    }

    /// Assumption: merged_id is not an ancestor of node_id in the graph.
    fn merge_in_internal(
        &mut self,
        node_id: NodeIndex,
        merged_id: NodeIndex,
        path: &OpPath,
        merge_parent_dependencies: bool,
    ) -> Result<(), FederationError> {
        let (node, merged) = self.graph.index_twice_mut(node_id, merged_id);
        if merged.is_top_level() {
            return Err(FederationError::internal(
                "Shouldn't remove top level nodes",
            ));
        }

        let mutable_node = Arc::make_mut(node);
        if merged.must_preserve_selection_set {
            mutable_node.must_preserve_selection_set = true;
        }

        if path.is_empty() {
            mutable_node
                .selection_set_mut()
                .add_selections(&merged.selection_set.selection_set)?;
        } else {
            // The merged nodes might have some @include/@skip at top-level that are already part of the path. If so,
            // we clean things up a bit.
            let merged_selection_set = remove_unneeded_top_level_fragment_directives(
                &merged.selection_set.selection_set,
                &path.conditional_directives(),
            )?;
            mutable_node
                .selection_set_mut()
                .add_at_path(path, Some(&Arc::new(merged_selection_set)))?;
        }

        self.on_modification();
        self.relocate_children_on_merged_in(node_id, merged_id, path);
        if merge_parent_dependencies {
            self.relocate_parents_on_merged_in(node_id, merged_id);
        }

        self.remap_reduced_defer_edges(merged_id, node_id);
        self.remove_node(merged_id);
        Ok(())
    }

    /// Merges `merged_id` into `node_id`, without knowing the dependencies between those two nodes.
    /// - Both `node_id` and `merged_id` must be in the same subgraph and have the same `merge_at`.
    // Note that it is up to the caller to know if such merging is desirable. In particular, if
    // both nodes have completely different inputs, merging them, which also merges their
    // dependencies, might not be judicious for the optimality of the query plan.
    // Assumptions:
    // - node_id's defer_ref == merged_id's defer_ref
    // - node_id's subgraph_name == merged_id's subgraph_name
    // - node_id's merge_at == merged_id's merge_at
    // - merged_id is not an ancestor of node_id in the graph.
    fn merge_in_with_all_dependencies(
        &mut self,
        node_id: NodeIndex,
        merged_id: NodeIndex,
    ) -> Result<(), FederationError> {
        self.copy_inputs(node_id, merged_id)?;
        self.merge_in_internal(
            node_id,
            merged_id,
            &OpPath::default(),
            /*merge_parent_dependencies*/ true,
        )
    }

    fn relocate_children_on_merged_in(
        &mut self,
        node_id: NodeIndex,
        merged_id: NodeIndex,
        path_in_this: &OpPath,
    ) {
        let mut new_parent_relations = IndexMap::default();
        for child_id in self.children_of(merged_id) {
            // This could already be a child of `this`. Typically, we can have case where we have:
            //     1
            //   /  \
            // 0     3
            //   \  /
            //     2
            // and we can merge siblings 2 into 1.
            if self.is_parent_of(node_id, child_id) {
                continue;
            }

            let path_in_merged = self
                .parent_relation(child_id, merged_id)
                .and_then(|r| r.path_in_parent);
            let concatenated_paths =
                concat_paths_in_parents(&Some(Arc::new(path_in_this.clone())), &path_in_merged);
            new_parent_relations.insert(
                child_id,
                ParentRelation {
                    parent_node_id: node_id,
                    path_in_parent: concatenated_paths,
                },
            );
        }
        for (child_id, new_parent) in new_parent_relations {
            self.add_parent(child_id, new_parent);
        }
    }

    fn relocate_parents_on_merged_in(&mut self, node_id: NodeIndex, merged_id: NodeIndex) {
        let mut new_parent_relations = Vec::new();
        for parent in self.parents_relations_of(merged_id) {
            // If the parent of the merged is already a parent of ours, don't re-create the already existing relationship.
            if self.is_parent_of(parent.parent_node_id, node_id) {
                continue;
            }

            // Further, if the parent is a descendant of `this`, we also should ignore that relationship, because
            // adding it a parent of `this` would create a cycle. And assuming this method is called properly,
            // that when `merged` can genuinely be safely merged into `this`, then this just mean the `parent` -> `merged`
            // relationship was unnecessary after all (which can happen given how groups are generated).
            if self.is_descendant_of(parent.parent_node_id, node_id) {
                continue;
            }
            new_parent_relations.push(parent.clone());
        }
        for new_parent in new_parent_relations {
            self.add_parent(node_id, new_parent);
        }
    }

    fn remove_inputs_from_selection(&mut self, node_id: NodeIndex) -> Result<(), FederationError> {
        let node = FetchDependencyGraph::node_weight_mut(&mut self.graph, node_id)?;
        node.remove_inputs_from_selection()?;
        Ok(())
    }

    fn is_node_unneeded(
        &self,
        node_id: NodeIndex,
        parent_relation: &ParentRelation,
    ) -> Result<bool, FederationError> {
        let node = self.node_weight(node_id)?;
        let parent = self.node_weight(parent_relation.parent_node_id)?;
        let Some(parent_op_path) = &parent_relation.path_in_parent else {
            return Ok(false);
        };
        let type_at_path = self.type_at_path(
            &parent.selection_set.selection_set.type_position,
            &parent.selection_set.selection_set.schema,
            parent_op_path,
        )?;
        let node_is_unneeded = node
            .selection_set
            .selection_set
            .can_rebase_on(&type_at_path, &parent.selection_set.selection_set.schema)?;
        Ok(node_is_unneeded)
    }

    fn type_at_path(
        &self,
        parent_type: &CompositeTypeDefinitionPosition,
        schema: &ValidFederationSchema,
        path: &Arc<OpPath>,
    ) -> Result<CompositeTypeDefinitionPosition, FederationError> {
        let mut type_ = parent_type.clone();
        for element in path.0.iter() {
            match &**element {
                OpPathElement::Field(field) => {
                    let field_position = type_.field(field.name().clone())?;
                    let field_definition = field_position.get(schema.schema())?;
                    let field_type = field_definition.ty.inner_named_type();
                    type_ = schema
                        .get_type(field_type)?
                        .try_into()
                        .map_or_else(
                            |_| {
                                Err(FederationError::internal(format!(
                                    "Invalid call from {path} starting at {parent_type}: {field_position} is not composite"
                                )))
                            },
                            Ok,
                        )?;
                }
                OpPathElement::InlineFragment(fragment) => {
                    if let Some(type_condition_position) = &fragment.type_condition_position {
                        type_ = schema
                            .get_type(type_condition_position.type_name())?
                            .try_into()
                            .map_or_else(
                                |_| {
                                    Err(FederationError::internal(format!(
                                        "Invalid call from {path} starting at {parent_type}: {type_condition_position} is not composite"
                                    )))
                                },
                                Ok,
                            )?;
                    } else {
                        continue;
                    }
                }
            }
        }
        Ok(type_)
    }
}

impl std::fmt::Display for FetchDependencyGraph {
    /// Displays the relationship between subgraph fetches.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn fmt_node(
            g: &FetchDependencyGraph,
            node_id: NodeIndex,
            f: &mut std::fmt::Formatter<'_>,
            indent: usize,
        ) -> std::fmt::Result {
            let Ok(node) = g.node_weight(node_id) else {
                return Ok(());
            };
            for _ in 0..indent {
                write!(f, "  ")?;
            }
            write!(f, "{} <- ", node.display(node_id))?;
            for (i, child_id) in g.children_of(node_id).enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }

                let Ok(child) = g.node_weight(child_id) else {
                    continue;
                };
                write!(f, "{}", child.subgraph_name)?;
            }

            if g.children_of(node_id).next().is_some() {
                f.write_char('\n')?;
            }

            for child_id in g.children_of(node_id) {
                fmt_node(g, child_id, f, indent + 1)?;
                f.write_char('\n')?;
            }
            Ok(())
        }

        for (i, &node_id) in self.root_nodes_by_subgraph.values().enumerate() {
            if i > 0 {
                f.write_char('\n')?;
            }
            fmt_node(self, node_id, f, 0)?;
        }
        Ok(())
    }
}

// Necessary for `petgraph::dot::Dot::with_attr_getters` calls to compile, but not executed.
impl std::fmt::Display for FetchDependencyGraphNode {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Err(std::fmt::Error)
    }
}

// Necessary for `petgraph::dot::Dot::with_attr_getters` calls to compile, but not executed.
impl std::fmt::Display for FetchDependencyGraphEdge {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Err(std::fmt::Error)
    }
}

impl FetchDependencyGraph {
    // NOTE: This method is not used during query planning. Rather, it is used during debugging.
    #[allow(dead_code)]
    /// GraphViz output for FetchDependencyGraph
    pub(crate) fn to_dot(&self) -> String {
        fn label_node(node_id: NodeIndex, node: &FetchDependencyGraphNode) -> String {
            let label_str = node.multiline_display(node_id).to_string();
            format!("label=\"{}\"", label_str.replace('"', "\\\""))
        }

        fn label_edge(edge_id: EdgeIndex) -> String {
            format!("label=\"{}\"", edge_id.index())
        }

        let config = [
            petgraph::dot::Config::NodeNoLabel,
            petgraph::dot::Config::EdgeNoLabel,
        ];
        petgraph::dot::Dot::with_attr_getters(
            &self.graph,
            &config,
            &(|_, er| label_edge(er.id())),
            &(|_, (node_id, node)| label_node(node_id, node)),
        )
        .to_string()
    }
}

impl FetchDependencyGraphNode {
    pub(crate) fn selection_set_mut(&mut self) -> &mut FetchSelectionSet {
        self.cached_cost = None;
        &mut self.selection_set
    }

    fn add_inputs(
        &mut self,
        selection: &SelectionSet,
        rewrites: impl IntoIterator<Item = Arc<FetchDataRewrite>>,
    ) -> Result<(), FederationError> {
        let inputs = self
            .inputs
            .get_or_insert_with(|| Arc::new(FetchInputs::empty(selection.schema.clone())));
        Arc::make_mut(inputs).add(selection)?;
        self.on_inputs_updated();
        Arc::make_mut(&mut self.input_rewrites).extend(rewrites);
        Ok(())
    }

    fn add_input_context(&mut self, context: Name, ty: Node<Type>) -> Result<(), FederationError> {
        let Some(inputs) = &mut self.inputs else {
            bail!("Shouldn't try to add inputs to a root fetch node")
        };
        Arc::make_mut(inputs).add_context(context, ty);
        Ok(())
    }

    fn copy_inputs(&mut self, other: &FetchDependencyGraphNode) -> Result<(), FederationError> {
        if let Some(other_inputs) = other.inputs.clone() {
            let inputs = self.inputs.get_or_insert_with(|| {
                Arc::new(FetchInputs::empty(other_inputs.supergraph_schema.clone()))
            });
            Arc::make_mut(inputs).add_all(&other_inputs)?;
            self.on_inputs_updated();

            let input_rewrites = Arc::make_mut(&mut self.input_rewrites);
            for rewrite in other.input_rewrites.iter() {
                input_rewrites.push(rewrite.clone());
            }

            for context_input in &other.context_inputs {
                self.add_context_renamer(context_input.clone());
            }
        }
        Ok(())
    }

    fn remove_inputs_from_selection(&mut self) -> Result<(), FederationError> {
        if let Some(inputs) = &mut self.inputs {
            self.cached_cost = None;
            let fetch_selection_set = &mut self.selection_set;
            for (_, selection) in &inputs.selection_sets_per_parent_type {
                fetch_selection_set.selection_set =
                    Arc::new(fetch_selection_set.selection_set.minus(selection)?);
            }
        }
        Ok(())
    }

    fn is_top_level(&self) -> bool {
        self.merge_at.is_none()
    }

    // PORT_NOTE: This corresponds to the `GroupInputs.onUpdateCallback` in the JS codebase.
    //            The callback is an optional value that is set only if the `inputs` is non-null
    //            in the `FetchGroup` constructor.
    //            In Rust version, the `self.inputs` is checked every time the `inputs` is updated,
    //            assuming `self.inputs` won't be changed from None to Some in the middle of its
    //            lifetime.
    fn on_inputs_updated(&mut self) {
        if self.inputs.is_some() {
            // (Original comment from the JS codebase with a minor adjustment for Rust version):
            // We're trying to avoid the full recomputation of `is_useless` when we're already
            // shown that the node is known useful (if it is shown useless, the node is removed,
            // so we're not caching that result but it's ok). And `is_useless` basically checks if
            // `inputs.contains(selection)`, so if a group is shown useful, it means that there
            // is some selections not in the inputs, but as long as we add to selections (and we
            // never remove from selections), then this won't change. Only changing inputs may
            // require some recomputation.
            self.is_known_useful = false;
        }
    }

    pub(crate) fn cost(&mut self) -> QueryPlanCost {
        if self.cached_cost.is_none() {
            self.cached_cost = Some(self.selection_set.selection_set.cost(1.0))
        }
        self.cached_cost.unwrap()
    }

    pub(crate) fn to_plan_node(
        &self,
        query_graph: &QueryGraph,
        handled_conditions: &Conditions,
        variable_definitions: &[Node<VariableDefinition>],
        operation_directives: &DirectiveList,
        operation_compression: &mut SubgraphOperationCompression,
        operation_name: Option<Name>,
    ) -> Result<Option<super::PlanNode>, FederationError> {
        if self.selection_set.selection_set.selections.is_empty() {
            return Ok(None);
        }
        let context_variable_definitions = self.inputs.iter().flat_map(|inputs| {
            inputs.used_contexts.iter().map(|(context, ty)| {
                Node::new(VariableDefinition {
                    name: context.clone(),
                    ty: ty.clone(),
                    default_value: None,
                    directives: Default::default(),
                })
            })
        });
        let variable_definitions = variable_definitions
            .iter()
            .cloned()
            .chain(context_variable_definitions)
            .collect::<Vec<_>>();
        let (selection, output_rewrites) =
            self.finalize_selection(&variable_definitions, handled_conditions)?;
        let input_nodes = self
            .inputs
            .as_ref()
            .map(|inputs| {
                inputs.to_selection_set_nodes(
                    &variable_definitions,
                    handled_conditions,
                    &self.parent_type,
                )
            })
            .transpose()?;
        let subgraph_schema = query_graph.schema_by_source(&self.subgraph_name)?;

        // Narrow down the variable definitions to only the ones used in the subgraph operation.
        let variable_definitions = {
            let mut collector = VariableCollector::new();
            collector.visit_directive_list(operation_directives);
            collector.visit_selection_set(&selection);
            let used_variables = collector.into_inner();

            variable_definitions
                .iter()
                .filter(|variable| used_variables.contains(&variable.name))
                .cloned()
                .collect::<Vec<_>>()
        };
        let variable_usages = {
            let mut list = variable_definitions
                .iter()
                .map(|var_def| var_def.name.clone())
                .collect::<Vec<_>>();
            list.sort();
            list
        };

        let operation = if self.is_entity_fetch {
            operation_for_entities_fetch(
                subgraph_schema,
                selection,
                variable_definitions,
                operation_directives,
                &operation_name,
            )?
        } else {
            operation_for_query_fetch(
                subgraph_schema,
                self.root_kind,
                selection,
                variable_definitions,
                operation_directives,
                &operation_name,
            )?
        };
        let operation_document = operation_compression.compress(operation)?;

        // this function removes unnecessary pieces of the query plan requires selection set.
        // PORT NOTE: this function was called trimSelectioNodes in the JS implementation
        fn trim_requires_selection_set(
            selection_set: &executable::SelectionSet,
        ) -> Vec<requires_selection::Selection> {
            selection_set
                .selections
                .iter()
                .filter_map(|s| match s {
                    executable::Selection::Field(field) => Some(
                        requires_selection::Selection::Field(requires_selection::Field {
                            alias: None,
                            name: field.name.clone(),
                            selections: trim_requires_selection_set(&field.selection_set),
                        }),
                    ),
                    executable::Selection::InlineFragment(inline_fragment) => {
                        Some(requires_selection::Selection::InlineFragment(
                            requires_selection::InlineFragment {
                                type_condition: inline_fragment.type_condition.clone(),
                                selections: trim_requires_selection_set(
                                    &inline_fragment.selection_set,
                                ),
                            },
                        ))
                    }
                    executable::Selection::FragmentSpread(_) => None,
                })
                .collect()
        }
        let node = super::PlanNode::Fetch(Box::new(super::FetchNode {
            subgraph_name: self.subgraph_name.clone(),
            id: self.id.get().copied(),
            variable_usages,
            requires: input_nodes
                .as_ref()
                .map(executable::SelectionSet::try_from)
                .transpose()?
                .map(|selection_set| trim_requires_selection_set(&selection_set))
                .unwrap_or_default(),
            operation_document: SerializableDocument::from_parsed(operation_document),
            operation_name,
            operation_kind: self.root_kind.into(),
            input_rewrites: self.input_rewrites.clone(),
            output_rewrites,
            context_rewrites: self
                .context_inputs
                .iter()
                .cloned()
                .map(|r| Arc::new(r.into()))
                .collect(),
        }));

        Ok(Some(if let Some(path) = self.merge_at.clone() {
            super::PlanNode::Flatten(super::FlattenNode {
                path,
                node: Box::new(node),
            })
        } else {
            node
        }))
    }

    // - `self.selection_set` must be fragment-spread-free.
    fn finalize_selection(
        &self,
        variable_definitions: &[Node<VariableDefinition>],
        handled_conditions: &Conditions,
    ) -> Result<(SelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
        // Finalizing the selection involves the following:
        // 1. removing any @include/@skip that are not necessary
        //    because they are already handled earlier in the query plan
        //    by some `ConditionNode`.
        // 2. adding __typename to all abstract types.
        //    This is because any follow-up fetch may need
        //    to select some of the entities fetched by this node,
        //    and so we need to have the __typename of those.
        // 3. checking if some selection violates
        //    `https://spec.graphql.org/draft/#FieldsInSetCanMerge()`:
        //    while the original query we plan for will never violate this,
        //    because the planner adds some additional fields to the query
        //    (due to @key and @requires) and because type-explosion changes the query,
        //    we could have violation of this.
        //    If that is the case, we introduce aliases to the selection to make it valid,
        //    and then generate a rewrite on the output of the fetch
        //    so that data aliased this way is rewritten back to the original/proper response name.
        let selection_without_conditions = remove_conditions_from_selection_set(
            &self.selection_set.selection_set,
            handled_conditions,
        )?;
        let selection_with_typenames =
            selection_without_conditions.add_typename_field_for_abstract_types(None)?;

        let (updated_selection, output_rewrites) =
            selection_with_typenames.add_aliases_for_non_merging_fields()?;

        updated_selection.validate(variable_definitions)?;
        Ok((updated_selection, output_rewrites))
    }

    /// Return a concise display for this node. The node index in the graph
    /// must be passed in externally.
    fn display(&self, index: NodeIndex) -> impl std::fmt::Display {
        use std::fmt;
        use std::fmt::Display;
        use std::fmt::Formatter;

        struct DisplayList<'a, T: Display>(&'a [T]);
        impl<T: Display> Display for DisplayList<'_, T> {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                let mut iter = self.0.iter();
                if let Some(x) = iter.next() {
                    write!(f, "{x}")?;
                }
                for x in iter {
                    write!(f, "::{x}")?;
                }
                Ok(())
            }
        }

        struct FetchDependencyNodeDisplay<'a> {
            node: &'a FetchDependencyGraphNode,
            index: NodeIndex,
        }

        impl Display for FetchDependencyNodeDisplay<'_> {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "[{}]", self.index.index())?;
                if self.node.defer_ref.is_some() {
                    write!(f, "(deferred)")?;
                }
                if let Some(&id) = self.node.id.get() {
                    write!(f, "{{id: {id}}}")?;
                }

                write!(f, " {}", self.node.subgraph_name)?;

                match (self.node.merge_at.as_deref(), self.node.inputs.as_deref()) {
                    (Some(merge_at), Some(inputs)) => {
                        write!(
                            f,
                            // @(path::to::*::field)[{input1,input2} => { id }]
                            "@({})[{} => {}]",
                            DisplayList(merge_at),
                            inputs,
                            self.node.selection_set.selection_set
                        )?;
                    }
                    (Some(merge_at), None) => {
                        write!(
                            f,
                            // @(path::to::*::field)[{} => { id }]
                            "@({})[{{}} => {}]",
                            DisplayList(merge_at),
                            self.node.selection_set.selection_set
                        )?;
                    }
                    (None, _) => {
                        // [{ id }]
                        write!(f, "[{}]", self.node.selection_set.selection_set)?;
                    }
                }

                Ok(())
            }
        }

        FetchDependencyNodeDisplay { node: self, index }
    }

    // A variation of `fn display` with multiline output, which is more suitable for
    // GraphViz output.
    pub(crate) fn multiline_display(&self, index: NodeIndex) -> impl std::fmt::Display {
        use std::fmt;
        use std::fmt::Display;
        use std::fmt::Formatter;

        struct DisplayList<'a, T: Display>(&'a [T]);
        impl<T: Display> Display for DisplayList<'_, T> {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                let mut iter = self.0.iter();
                if let Some(x) = iter.next() {
                    write!(f, "{x}")?;
                }
                for x in iter {
                    write!(f, "::{x}")?;
                }
                Ok(())
            }
        }

        struct FetchDependencyNodeDisplay<'a> {
            node: &'a FetchDependencyGraphNode,
            index: NodeIndex,
        }

        impl Display for FetchDependencyNodeDisplay<'_> {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "[{}]", self.index.index())?;
                if self.node.defer_ref.is_some() {
                    write!(f, "(deferred)")?;
                }
                if let Some(&id) = self.node.id.get() {
                    write!(f, "{{id: {id}}}")?;
                }

                write!(f, " {}", self.node.subgraph_name)?;

                match (self.node.merge_at.as_deref(), self.node.inputs.as_deref()) {
                    (Some(merge_at), Some(inputs)) => {
                        write!(
                            f,
                            // @(path::to::*::field)[{input1,input2} => { id }]
                            "\n@({})\n{}\n=>\n{}\n",
                            DisplayList(merge_at),
                            inputs,
                            self.node.selection_set.selection_set
                        )?;
                    }
                    (Some(merge_at), None) => {
                        write!(
                            f,
                            // @(path::to::*::field)[{} => { id }]
                            "\n@({})\n{{}}\n=>\n{}\n",
                            DisplayList(merge_at),
                            self.node.selection_set.selection_set
                        )?;
                    }
                    (None, _) => {
                        // [(type){ id }]
                        write!(
                            f,
                            "\n({})\n{}",
                            self.node.parent_type, self.node.selection_set.selection_set
                        )?;
                    }
                }

                Ok(())
            }
        }

        FetchDependencyNodeDisplay { node: self, index }
    }

    // PORT_NOTE: In JS version, this value is memoized on the node struct.
    // Uses a numeric hash instead of the JS string key to avoid per-node
    // string allocation + formatting. Hash collisions are safe because the
    // caller (merge_fetches_with_same_subgraph_and_merge_at) does a full
    // equality check via has_equal_inputs within each bucket.
    fn subgraph_and_merge_at_key(&self) -> Option<u64> {
        self.inputs.as_ref()?;
        let mut hasher = ahash::AHasher::default();
        self.subgraph_name.hash(&mut hasher);
        self.merge_at.hash(&mut hasher);
        Some(hasher.finish())
    }

    fn add_context_renamer(&mut self, renamer: FetchDataKeyRenamer) {
        // XXX(@goto-bus-stop): this looks like it should be an IndexSet!
        if !self.context_inputs.contains(&renamer) {
            self.context_inputs.push(renamer);
        }
    }

    fn add_context_renamers_for_selection_set(
        &mut self,
        selection_set: Option<&SelectionSet>,
        relative_path: Vec<FetchDataPathElement>,
        alias: Name,
    ) -> Result<(), FederationError> {
        let selection_set = match selection_set {
            Some(selection_set) if !selection_set.is_empty() => selection_set,
            _ => {
                self.add_context_renamer(FetchDataKeyRenamer {
                    path: relative_path,
                    rename_key_to: alias,
                });
                return Ok(());
            }
        };

        for selection in selection_set {
            match selection {
                Selection::Field(field_selection) => {
                    if matches!(relative_path.last(), Some(FetchDataPathElement::Parent))
                        && selection_set.type_position.type_name() != "Query"
                    {
                        for possible_runtime_type in selection_set
                            .schema
                            .possible_runtime_types(selection_set.type_position.clone())?
                        {
                            let mut new_relative_path = relative_path.clone();
                            new_relative_path.push(FetchDataPathElement::TypenameEquals(
                                possible_runtime_type.type_name.clone(),
                            ));
                            self.add_context_renamers_for_selection_set(
                                Some(selection_set),
                                new_relative_path,
                                alias.clone(),
                            )?;
                        }
                    } else {
                        let mut new_relative_path = relative_path.clone();
                        new_relative_path.push(FetchDataPathElement::Key(
                            field_selection.field.field_position.field_name().clone(),
                            Default::default(),
                        ));
                        self.add_context_renamers_for_selection_set(
                            field_selection.selection_set.as_ref(),
                            new_relative_path,
                            alias.clone(),
                        )?;
                    }
                }
                Selection::InlineFragment(inline_fragment_selection) => {
                    if let Some(type_condition) = &inline_fragment_selection
                        .inline_fragment
                        .type_condition_position
                    {
                        let mut new_relative_path = relative_path.clone();
                        new_relative_path.push(FetchDataPathElement::TypenameEquals(
                            type_condition.type_name().clone(),
                        ));
                        self.add_context_renamers_for_selection_set(
                            Some(&inline_fragment_selection.selection_set),
                            new_relative_path,
                            alias.clone(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Toposort a `StableDiGraph` and convert it into a dense `petgraph::adj::List`
/// suitable for `dag_transitive_reduction_closure`.
///
/// Returns `None` if the graph contains a cycle. Otherwise returns the
/// adjacency list and a rank-of lookup (`tred_index_of[node.index()] → toposort
/// rank`) so the caller can map results back to `NodeIndex` values.
fn to_toposorted_adjacency_list<N, E>(
    graph: &StableDiGraph<N, E>,
) -> Option<(AdjList<(), u32>, Vec<usize>)> {
    let sorted = petgraph::algo::toposort(graph, None).ok()?;
    let node_count = sorted.len();
    let node_bound = graph.node_bound();

    let mut tred_index_of: Vec<usize> = vec![usize::MAX; node_bound];
    for (rank, &node) in sorted.iter().enumerate() {
        tred_index_of[node.index()] = rank;
    }

    let mut adj: AdjList<(), u32> = AdjList::with_capacity(node_count);
    for _ in 0..node_count {
        adj.add_node();
    }
    for &node in &sorted {
        let node_tred_idx = tred_index_of[node.index()];
        for parent in graph.neighbors_directed(node, petgraph::Direction::Incoming) {
            let parent_tred_idx = tred_index_of[parent.index()];
            adj.add_edge(
                petgraph::adj::NodeIndex::new(parent_tred_idx),
                petgraph::adj::NodeIndex::new(node_tred_idx),
                (),
            );
        }
    }

    Some((adj, tred_index_of))
}

fn operation_for_entities_fetch(
    subgraph_schema: &ValidFederationSchema,
    selection_set: SelectionSet,
    mut variable_definitions: Vec<Node<VariableDefinition>>,
    operation_directives: &DirectiveList,
    operation_name: &Option<Name>,
) -> Result<Operation, FederationError> {
    variable_definitions.insert(0, representations_variable_definition(subgraph_schema)?);

    let query_type_name = subgraph_schema.schema().root_operation(OperationType::Query).ok_or_else(||
    SingleFederationError::InvalidSubgraph {
        message: "Subgraphs should always have a query root (they should at least provides _entities)".to_string()
    })?;

    let query_type = match subgraph_schema.get_type(query_type_name)? {
        TypeDefinitionPosition::Object(o) => o,
        _ => {
            return Err(SingleFederationError::InvalidSubgraph {
                message: "the root query type must be an object".to_string(),
            }
            .into());
        }
    };

    if !query_type
        .get(subgraph_schema.schema())?
        .fields
        .contains_key(&ENTITIES_QUERY)
    {
        return Err(SingleFederationError::InvalidSubgraph {
            message: "Subgraphs should always have the _entities field".to_string(),
        }
        .into());
    }

    let entities = FieldDefinitionPosition::Object(query_type.field(ENTITIES_QUERY));

    let entities_call = Selection::from_element(
        OpPathElement::Field(Field {
            schema: subgraph_schema.clone(),
            field_position: entities,
            alias: None,
            arguments: ArgumentList::one((
                FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME,
                executable::Value::Variable(FEDERATION_REPRESENTATIONS_VAR_NAME),
            )),
            directives: Default::default(),
            sibling_typename: None,
        }),
        Some(selection_set),
    )?;

    let type_position: CompositeTypeDefinitionPosition =
        subgraph_schema.get_type(query_type_name)?.try_into()?;

    let mut map = SelectionMap::new();
    map.insert(entities_call);

    let selection_set = SelectionSet {
        schema: subgraph_schema.clone(),
        type_position,
        selections: Arc::new(map),
    };

    Ok(Operation {
        schema: subgraph_schema.clone(),
        root_kind: SchemaRootDefinitionKind::Query,
        name: operation_name.clone(),
        variables: Arc::new(variable_definitions),
        directives: operation_directives.clone(),
        selection_set,
    })
}

fn operation_for_query_fetch(
    subgraph_schema: &ValidFederationSchema,
    root_kind: SchemaRootDefinitionKind,
    selection_set: SelectionSet,
    variable_definitions: Vec<Node<VariableDefinition>>,
    operation_directives: &DirectiveList,
    operation_name: &Option<Name>,
) -> Result<Operation, FederationError> {
    Ok(Operation {
        schema: subgraph_schema.clone(),
        root_kind,
        name: operation_name.clone(),
        variables: Arc::new(variable_definitions),
        directives: operation_directives.clone(),
        selection_set,
    })
}

fn representations_variable_definition(
    schema: &ValidFederationSchema,
) -> Result<Node<VariableDefinition>, FederationError> {
    let _metadata = schema
        .metadata()
        .ok_or_else(|| FederationError::internal("Expected schema to be a federation subgraph"))?;

    let any_name = schema.federation_type_name_in_schema(ANY_SCALAR_NAME)?;

    Ok(VariableDefinition {
        name: FEDERATION_REPRESENTATIONS_VAR_NAME,
        ty: Type::Named(any_name).non_null().list().non_null().into(),
        default_value: None,
        directives: Default::default(),
    }
    .into())
}

impl SelectionSet {
    pub(crate) fn cost(&self, depth: QueryPlanCost) -> QueryPlanCost {
        // The cost is essentially the number of elements in the selection,
        // but we make deep element cost a tiny bit more,
        // mostly to make things a tad more deterministic
        // (typically, if we have an interface with a single implementation,
        // then we can have a choice between a query plan that type-explode a field of the interface
        // and one that doesn't, and both will be almost identical,
        // except that the type-exploded field will be a different depth;
        // by favoring lesser depth in that case, we favor not type-exploding).
        self.selections.values().fold(0.0, |sum, selection| {
            let subselections = match selection {
                Selection::Field(field) => field.selection_set.as_ref(),
                Selection::InlineFragment(inline) => Some(&inline.selection_set),
            };
            let subselections_cost = if let Some(selection_set) = subselections {
                selection_set.cost(depth + 1.0)
            } else {
                0.0
            };
            sum + depth + subselections_cost
        })
    }
}

impl FetchSelectionSet {
    pub(crate) fn empty(
        schema: ValidFederationSchema,
        type_position: CompositeTypeDefinitionPosition,
    ) -> Result<Self, FederationError> {
        let selection_set = Arc::new(SelectionSet::empty(schema, type_position));
        Ok(Self {
            conditions: OnceLock::new(),
            selection_set,
        })
    }

    fn add_at_path(
        &mut self,
        path_in_node: &OpPath,
        selection_set: Option<&Arc<SelectionSet>>,
    ) -> Result<(), FederationError> {
        Arc::make_mut(&mut self.selection_set).add_at_path(path_in_node, selection_set)?;
        self.conditions.take();
        Ok(())
    }

    fn add_selections(&mut self, selection_set: &Arc<SelectionSet>) -> Result<(), FederationError> {
        Arc::make_mut(&mut self.selection_set).add_selection_set(selection_set)?;
        self.conditions.take();
        Ok(())
    }

    /// The conditions determining whether the fetch should be executed.
    fn conditions(&self) -> Result<&Conditions, FederationError> {
        // This is a bit inefficient, because `get_or_try_init` is unstable.
        // https://github.com/rust-lang/rust/issues/109737
        //
        // Essentially we do `.get()` twice. This is still much better than eagerly recomputing the
        // selection set all the time, though :)
        if let Some(conditions) = self.conditions.get() {
            return Ok(conditions);
        }

        // Separating this call and the `.get_or_init` call means we could, if called from multiple
        // threads, do the same work twice.
        // The query planner does not use multiple threads for a single plan at the moment, and
        // even if it did, occasionally computing this twice would still be better than eagerly
        // recomputing it after every change.
        let conditions = self.selection_set.conditions()?;
        Ok(self.conditions.get_or_init(|| conditions))
    }
}

impl FetchInputs {
    pub(crate) fn empty(supergraph_schema: ValidFederationSchema) -> Self {
        Self {
            selection_sets_per_parent_type: Default::default(),
            supergraph_schema,
            used_contexts: Default::default(),
        }
    }

    fn add(&mut self, selection: &SelectionSet) -> Result<(), FederationError> {
        assert_eq!(
            selection.schema, self.supergraph_schema,
            "Inputs selections must be based on the supergraph schema"
        );
        let type_selections = self
            .selection_sets_per_parent_type
            .entry(selection.type_position.clone())
            .or_insert_with(|| {
                Arc::new(SelectionSet::empty(
                    selection.schema.clone(),
                    selection.type_position.clone(),
                ))
            });
        Arc::make_mut(type_selections).add_local_selection_set(selection)
        // PORT_NOTE: `onUpdateCallback` call is moved to `FetchDependencyGraphNode::on_inputs_updated`.
    }

    fn add_all(&mut self, other: &Self) -> Result<(), FederationError> {
        other
            .selection_sets_per_parent_type
            .values()
            .try_for_each(|selections| self.add(selections))?;
        other
            .used_contexts
            .iter()
            .for_each(|(context, ty)| self.add_context(context.clone(), ty.clone()));
        Ok(())
    }

    fn contains(&self, other: &Self) -> bool {
        for (parent_type, other_selection) in &other.selection_sets_per_parent_type {
            let Some(this_selection) = self.selection_sets_per_parent_type.get(parent_type) else {
                return false;
            };
            if !this_selection.contains(other_selection) {
                return false;
            }
        }
        if self.used_contexts.len() < other.used_contexts.len() {
            return false;
        }
        other
            .used_contexts
            .keys()
            .all(|context| self.used_contexts.contains_key(context))
    }

    fn equals(&self, other: &Self) -> bool {
        if self.selection_sets_per_parent_type.len() != other.selection_sets_per_parent_type.len() {
            return false;
        }

        // For all parent types in `self`, its selection set is equal to that of the `other`.
        // Since they have the same # of parent types, the other way around should also hold.
        for (parent_type, self_selections) in &self.selection_sets_per_parent_type {
            let Some(other_selections) = other.selection_sets_per_parent_type.get(parent_type)
            else {
                return false;
            };
            if !self_selections
                .containment(other_selections, ContainmentOptions::default())
                .is_equal()
            {
                return false;
            }
            // so far so good
        }
        if self.used_contexts.len() != other.used_contexts.len() {
            return false;
        }
        other
            .used_contexts
            .keys()
            .all(|context| self.used_contexts.contains_key(context))
    }

    fn to_selection_set_nodes(
        &self,
        variable_definitions: &[Node<VariableDefinition>],
        handled_conditions: &Conditions,
        type_position: &CompositeTypeDefinitionPosition,
    ) -> Result<SelectionSet, FederationError> {
        let mut selections = SelectionMap::new();
        for selection_set in self.selection_sets_per_parent_type.values() {
            let selection_set =
                remove_conditions_from_selection_set(selection_set, handled_conditions)?;
            // Making sure we're not generating something invalid.
            selection_set.validate(variable_definitions)?;
            selections.extend_ref(&selection_set.selections)
        }
        Ok(SelectionSet {
            schema: self.supergraph_schema.clone(),
            type_position: type_position.clone(),
            selections: Arc::new(selections),
        })
    }

    fn add_context(&mut self, context: Name, ty: Node<Type>) {
        self.used_contexts.insert(context, ty);
    }
}

impl std::fmt::Display for FetchInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.selection_sets_per_parent_type.len() {
            0 => f.write_str("{}"),
            1 => write!(
                f,
                "{}",
                // We can safely unwrap because we know the len >= 1.
                self.selection_sets_per_parent_type.values().next().unwrap()
            ),
            2.. => {
                write!(f, "[")?;
                let mut iter = self.selection_sets_per_parent_type.values();
                // We can safely unwrap because we know the len >= 1.
                write!(f, "{}", iter.next().unwrap())?;
                for x in iter {
                    write!(f, ",{x}")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl DeferTracking {
    fn empty(
        schema: &ValidFederationSchema,
        root_type_for_defer: Option<CompositeTypeDefinitionPosition>,
    ) -> Self {
        Self {
            top_level_deferred: Default::default(),
            deferred: Default::default(),
            primary_selection: root_type_for_defer
                .map(|type_position| SelectionSet::empty(schema.clone(), type_position)),
        }
    }

    fn register_defer(
        &mut self,
        defer_context: &DeferContext,
        defer_args: &DeferDirectiveArguments,
        path: FetchDependencyGraphNodePath,
        parent_type: CompositeTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        // Having the primary selection undefined means that @defer handling is actually disabled, so there's no need to track anything.
        let Some(primary_selection) = self.primary_selection.as_mut() else {
            return Ok(());
        };

        let label = defer_args
            .label
            .as_ref()
            .expect("All @defer should have been labeled at this point");

        self.deferred.entry(label.clone()).or_insert_with(|| {
            DeferredInfo::empty(
                primary_selection.schema.clone(),
                label.clone(),
                path,
                parent_type.clone(),
            )
        });

        if let Some(parent_ref) = &defer_context.current_defer_ref {
            let Some(parent_info) = self.deferred.get_mut(parent_ref) else {
                bail!("Cannot find info for parent {parent_ref} or {label}")
            };

            parent_info.deferred.insert(label.clone());
            parent_info
                .sub_selection
                .add_at_path(&defer_context.path_to_defer_parent, None)
        } else {
            self.top_level_deferred.insert(label.clone());
            primary_selection.add_at_path(&defer_context.path_to_defer_parent, None)
        }
    }

    fn update_subselection(
        &mut self,
        defer_context: &DeferContext,
        selection_set: Option<&Arc<SelectionSet>>,
    ) -> Result<(), FederationError> {
        if !defer_context.is_part_of_query {
            return Ok(());
        }
        let Some(primary_selection) = &mut self.primary_selection else {
            return Ok(());
        };
        if let Some(parent_ref) = &defer_context.current_defer_ref {
            self.deferred[parent_ref]
                .sub_selection
                .add_at_path(&defer_context.path_to_defer_parent, selection_set)
        } else {
            primary_selection.add_at_path(&defer_context.path_to_defer_parent, selection_set)
        }
    }

    fn add_dependency(&mut self, label: &str, id_dependency: DeferRef) {
        let info = self
            .deferred
            .get_mut(label)
            .expect("Cannot find info for label");
        info.dependencies.insert(id_dependency);
    }

    // PORT_NOTE: this probably should just return labels and not the whole DeferredInfo
    // to make it a bit easier to work with, since at the usage site, the return value
    // is iterated over while also mutating the fetch dependency graph, which is mutually exclusive
    // with holding a reference to a DeferredInfo. For now we just clone the return value when
    // necessary.
    fn defers_in_parent<'s>(&'s self, parent_ref: Option<&str>) -> Vec<&'s DeferredInfo> {
        let labels = match parent_ref {
            Some(parent_ref) => {
                let Some(info) = self.deferred.get(parent_ref) else {
                    return vec![];
                };
                &info.deferred
            }
            None => &self.top_level_deferred,
        };

        labels
            .iter()
            .map(|label| {
                self.deferred
                    .get(label)
                    .expect("referenced defer label without existing info")
            })
            .collect()
    }
}

impl DeferredInfo {
    fn empty(
        schema: ValidFederationSchema,
        label: DeferRef,
        path: FetchDependencyGraphNodePath,
        parent_type: CompositeTypeDefinitionPosition,
    ) -> Self {
        Self {
            label,
            path,
            sub_selection: SelectionSet::empty(schema, parent_type),
            deferred: Default::default(),
            dependencies: Default::default(),
        }
    }
}

struct ComputeNodesStackItem<'a> {
    tree: &'a OpPathTree,
    node_id: NodeIndex,
    node_path: FetchDependencyGraphNodePath,
    context: &'a OpGraphPathContext,
    defer_context: DeferContext,
    context_to_condition_nodes: Arc<IndexMap<Name, Vec<NodeIndex>>>,
}

#[cfg_attr(
    feature = "snapshot_tracing",
    tracing::instrument(skip_all, level = "trace")
)]
pub(crate) fn compute_nodes_for_tree(
    dependency_graph: &mut FetchDependencyGraph,
    initial_tree: &OpPathTree,
    initial_node_id: NodeIndex,
    initial_node_path: FetchDependencyGraphNodePath,
    initial_defer_context: DeferContext,
    initial_conditions: &OpGraphPathContext,
    check_cancellation: &dyn Fn() -> Result<(), SingleFederationError>,
) -> Result<IndexSet<NodeIndex>, FederationError> {
    snapshot!("OpPathTree", initial_tree.to_string(), "path_tree");
    let mut stack = vec![ComputeNodesStackItem {
        tree: initial_tree,
        node_id: initial_node_id,
        node_path: initial_node_path,
        context: initial_conditions,
        defer_context: initial_defer_context,
        context_to_condition_nodes: Arc::new(Default::default()),
    }];
    let mut created_nodes = IndexSet::default();
    while let Some(stack_item) = stack.pop() {
        check_cancellation()?;
        let node =
            FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
        for selection_set in &stack_item.tree.local_selection_sets {
            node.selection_set_mut()
                .add_at_path(&stack_item.node_path.path_in_node, Some(selection_set))?;
            dependency_graph
                .defer_tracking
                .update_subselection(&stack_item.defer_context, Some(selection_set))?;
        }
        if stack_item.tree.is_leaf() {
            node.selection_set_mut()
                .add_at_path(&stack_item.node_path.path_in_node, None)?;
            dependency_graph
                .defer_tracking
                .update_subselection(&stack_item.defer_context, None)?;
            continue;
        }
        // We want to preserve the order of the elements in the child,
        // but the stack will reverse everything,
        // so we iterate in reverse order to counter-balance it.
        for child in stack_item.tree.childs.iter().rev() {
            match &*child.trigger {
                OpGraphPathTrigger::Context(new_context) => {
                    // The only 3 cases where we can take edge not "driven" by an operation is either:
                    // * when we resolve a key
                    // * resolve a query (switch subgraphs because the query root type is the type of a field)
                    // * or at the root of subgraph graph.
                    // The latter case has already be handled the beginning of
                    // `QueryPlanningTraversal::updated_dependency_graph` so only the 2 former remains.
                    let Some(edge_id) = child.edge else {
                        return Err(FederationError::internal(format!(
                            "Unexpected 'null' edge with no trigger at {:?}",
                            stack_item.node_path
                        )));
                    };
                    let edge = stack_item.tree.graph.edge_weight(edge_id)?;
                    match edge.transition {
                        QueryGraphEdgeTransition::KeyResolution => {
                            stack.push(compute_nodes_for_key_resolution(
                                dependency_graph,
                                &stack_item,
                                child,
                                edge_id,
                                new_context,
                                &mut created_nodes,
                                check_cancellation,
                            )?);
                        }
                        QueryGraphEdgeTransition::RootTypeResolution { root_kind } => {
                            stack.push(compute_nodes_for_root_type_resolution(
                                dependency_graph,
                                &stack_item,
                                child,
                                edge_id,
                                edge,
                                root_kind,
                                new_context,
                            )?);
                        }
                        _ => {
                            return Err(FederationError::internal(format!(
                                "Unexpected non-collecting edge {edge}"
                            )));
                        }
                    }
                }
                OpGraphPathTrigger::OpPathElement(operation) => {
                    stack.push(compute_nodes_for_op_path_element(
                        dependency_graph,
                        &stack_item,
                        child,
                        operation,
                        &mut created_nodes,
                        check_cancellation,
                    )?);
                }
            }
        }
    }
    snapshot!(
        "FetchDependencyGraph",
        dependency_graph.to_dot(),
        "Fetch dependency graph updated by compute_nodes_for_tree"
    );
    Ok(created_nodes)
}

#[cfg_attr(
    feature = "snapshot_tracing",
    tracing::instrument(skip_all, level = "trace")
)]
fn compute_nodes_for_key_resolution<'a>(
    dependency_graph: &mut FetchDependencyGraph,
    stack_item: &ComputeNodesStackItem<'a>,
    child: &'a PathTreeChild<OpGraphPathTrigger, Option<EdgeIndex>>,
    edge_id: EdgeIndex,
    new_context: &'a OpGraphPathContext,
    created_nodes: &mut IndexSet<NodeIndex>,
    check_cancellation: &dyn Fn() -> Result<(), SingleFederationError>,
) -> Result<ComputeNodesStackItem<'a>, FederationError> {
    let edge = stack_item.tree.graph.edge_weight(edge_id)?;
    let Some(conditions) = &child.conditions else {
        return Err(FederationError::internal(format!(
            "Key edge {edge:?} should have some conditions paths",
        )));
    };
    // First, we need to ensure we fetch the conditions from the current node.
    let conditions_nodes = compute_nodes_for_tree(
        dependency_graph,
        conditions,
        stack_item.node_id,
        stack_item.node_path.clone(),
        stack_item.defer_context.for_conditions(),
        &Default::default(),
        check_cancellation,
    )?;
    created_nodes.extend(conditions_nodes.iter().copied());
    // Then we can "take the edge", creating a new node.
    // That node depends on the condition ones.
    let (source_id, dest_id) = stack_item.tree.graph.edge_endpoints(edge_id)?;
    let source = stack_item.tree.graph.node_weight(source_id)?;
    let dest = stack_item.tree.graph.node_weight(dest_id)?;
    // We shouldn't have a key on a non-composite type
    let source_type: CompositeTypeDefinitionPosition = source.type_.clone().try_into()?;
    let source_schema: ValidFederationSchema = dependency_graph
        .federated_query_graph
        .schema_by_source(&source.source)?
        .clone();
    let dest_type: CompositeTypeDefinitionPosition = dest.type_.clone().try_into()?;
    let dest_schema: ValidFederationSchema = dependency_graph
        .federated_query_graph
        .schema_by_source(&dest.source)?
        .clone();
    let path_in_parent = &stack_item.node_path.path_in_node;
    let updated_defer_context = stack_item.defer_context.after_subgraph_jump();
    // Note that we use the name of `dest_type` for the inputs parent type, which can seem strange,
    // but the reason is that we have 2 kind of cases:
    //  - either source_type == dest_type, which is the case for an object entity key,
    //    or for a key from an @interfaceObject to an interface key.
    //  - or source_type !== dest_type,
    //    and that means the source is an implementation type X of some interface I,
    //    and dest_type is an @interfaceObject corresponding to I.
    //    But in that case, using I as base for the inputs is a bit more flexible
    //    as it ensure that if the query uses multiple such key for multiple implementations
    //    (so, key from X to I, and then Y to I), then the same fetch is properly reused.
    //    Note that it is ok to do so since
    //    1) inputs are based on the supergraph schema, so I is going to exist there and
    //    2) we wrap the input selection properly against `source_type` below anyway.
    let new_node_id = dependency_graph.get_or_create_key_node(
        &dest.source,
        &stack_item.node_path.response_path,
        &dest_type,
        ParentRelation {
            parent_node_id: stack_item.node_id,
            path_in_parent: Some(Arc::clone(path_in_parent)),
        },
        &conditions_nodes,
        updated_defer_context.active_defer_ref.as_ref(),
    )?;
    created_nodes.insert(new_node_id);
    for condition_node in conditions_nodes {
        // If `condition_node` parent is `node_id`,
        // that is the same as `new_node_id` current parent,
        // then we can infer the path of `new_node_id` into that condition node
        // by looking at the paths of each to their common parent.
        // But otherwise, we cannot have a proper "path in parent".
        //
        // Note that the conditions are resolved starting from the very position of
        // `new_node_id` (see `compute_nodes_for_tree` above, which is passed
        // `stack_item.node_path`), so `condition_node`'s path into the common parent is always
        // `path_in_parent` possibly followed by a deeper suffix. A non-empty suffix means
        // `condition_node` sits *below* `new_node_id` in the data, and so there is no downward
        // path from `condition_node` to `new_node_id`: the suffix describes the reverse
        // relation and must not be used here. Only equal paths give us a usable (empty) path.
        //
        // Note that this is stricter than it strictly needs to be: if `condition_path` were a
        // *prefix* of `path_in_parent`, then `condition_node` would sit above `new_node_id` and
        // `path_in_parent.strip_prefix(condition_path)` would be a usable downward path. That case
        // is not known to be reachable and was not handled before either, so we leave it out.
        let mut path = None;
        let mut iter = dependency_graph.parents_relations_of(condition_node);
        if let (Some(condition_node_parent), None) = (iter.next(), iter.next()) {
            // There is exactly one parent
            if condition_node_parent.parent_node_id == stack_item.node_id
                && let Some(condition_path) = condition_node_parent.path_in_parent
                && condition_path == *path_in_parent
            {
                path = Some(Arc::new(OpPath::default()))
            }
        }
        drop(iter);
        dependency_graph.add_parent(
            new_node_id,
            ParentRelation {
                parent_node_id: condition_node,
                path_in_parent: path,
            },
        )
    }
    // Note that inputs must be based on the supergraph schema, not any particular subgraph,
    // since sometimes key conditions are fetched from multiple subgraphs
    // (and so no one subgraph has a type definition with all the proper fields,
    // only the supergraph does).
    let input_type = dependency_graph.type_for_fetch_inputs(source_type.type_name())?;
    let mut input_selections = SelectionSet::for_composite_type(
        dependency_graph.supergraph_schema.clone(),
        input_type.clone(),
    );
    let Some(edge_conditions) = &edge.conditions else {
        // PORT_NOTE: TypeScript `computeGroupsForTree()` has a non-null assertion here
        return Err(FederationError::internal(
            "missing expected edge conditions",
        ));
    };
    input_selections.add_selection_set(edge_conditions)?;

    let new_node = FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, new_node_id)?;
    new_node.add_inputs(
        &wrap_input_selections(
            &dependency_graph.supergraph_schema,
            &input_type,
            input_selections,
            new_context,
        ),
        compute_input_rewrites_on_key_fetch(input_type.type_name(), &dest_type, &dest_schema)?
            .into_iter()
            .flatten(),
    )?;

    // We also ensure to get the __typename of the current type in the "original" node.
    let node =
        FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
    let typename_field = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
        &source_schema,
        &source_type,
        None,
    )));
    let typename_path = stack_item
        .node_path
        .path_in_node
        .with_pushed(typename_field);
    node.selection_set_mut().add_at_path(&typename_path, None)?;
    Ok(ComputeNodesStackItem {
        tree: &child.tree,
        node_id: new_node_id,
        node_path: stack_item
            .node_path
            .for_new_key_fetch(create_fetch_initial_path(
                &dependency_graph.supergraph_schema,
                &dest_type,
                new_context,
            )?),
        context: new_context,
        defer_context: updated_defer_context,
        context_to_condition_nodes: stack_item.context_to_condition_nodes.clone(),
    })
}

#[cfg_attr(
    feature = "snapshot_tracing",
    tracing::instrument(skip_all, level = "trace")
)]
fn compute_nodes_for_root_type_resolution<'a>(
    dependency_graph: &mut FetchDependencyGraph,
    stack_item: &ComputeNodesStackItem<'_>,
    child: &'a Arc<PathTreeChild<OpGraphPathTrigger, Option<EdgeIndex>>>,
    edge_id: EdgeIndex,
    edge: &crate::query_graph::QueryGraphEdge,
    root_kind: SchemaRootDefinitionKind,
    new_context: &'a OpGraphPathContext,
) -> Result<ComputeNodesStackItem<'a>, FederationError> {
    if child.conditions.is_some() {
        return Err(FederationError::internal(format!(
            "Root type resolution edge {edge} should not have conditions"
        )));
    }
    let (source_id, dest_id) = stack_item.tree.graph.edge_endpoints(edge_id)?;
    let source = stack_item.tree.graph.node_weight(source_id)?;
    let dest = stack_item.tree.graph.node_weight(dest_id)?;
    let source_type: ObjectTypeDefinitionPosition = source.type_.clone().try_into()?;
    let source_schema: ValidFederationSchema = dependency_graph
        .federated_query_graph
        .schema_by_source(&source.source)?
        .clone();
    let dest_type: ObjectTypeDefinitionPosition = dest.type_.clone().try_into()?;
    let root_operation_type = dependency_graph
        .federated_query_graph
        .schema_by_source(&dest.source)?
        .schema()
        .root_operation(root_kind.into());
    if root_operation_type != Some(&dest_type.type_name) {
        return Err(FederationError::internal(format!(
            "Expected {dest_type} to be the root {root_kind} type, \
             but that is {root_operation_type:?}"
        )));
    }

    // Usually, we get here because a field (say `q`) has query root type as type,
    // and the field queried for that root type is on another subgraph.
    // When that happens, it means that on the original subgraph
    // we may not have added _any_ subselection for type `q`
    // and that would make the query to the original subgraph invalid.
    // To avoid this, we request the __typename field.
    // One exception however is if we're at the "top" of the current node
    // (`path_in_node.is_empty()`, which is a corner case but can happen with @defer
    // when everything in a query is deferred):
    // in that case, there is no point in adding __typename
    // because if we don't add any other selection, the node will be empty
    // and we've rather detect that and remove the node entirely later.
    let node =
        FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
    if !stack_item.node_path.path_in_node.is_empty() {
        let typename_field = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
            &source_schema,
            &source_type.into(),
            None,
        )));
        let typename_path = stack_item
            .node_path
            .path_in_node
            .with_pushed(typename_field);
        node.selection_set_mut().add_at_path(&typename_path, None)?;
    }

    // We take the edge, creating a new node.
    // Note that we always create a new node because this corresponds to jumping subgraph
    // after a field returned the query root type,
    // and we want to preserve this ordering somewhat (debatable, possibly).
    let updated_defer_context = stack_item.defer_context.after_subgraph_jump();
    let new_node_id = dependency_graph.new_root_type_node(
        dest.source.clone(),
        root_kind,
        &dest_type,
        Some(stack_item.node_path.response_path.clone()),
        updated_defer_context.active_defer_ref.clone(),
    )?;
    dependency_graph.add_parent(
        new_node_id,
        ParentRelation {
            parent_node_id: stack_item.node_id,
            path_in_parent: Some(Arc::clone(&stack_item.node_path.path_in_node)),
        },
    );
    Ok(ComputeNodesStackItem {
        tree: &child.tree,
        node_id: new_node_id,
        node_path: stack_item
            .node_path
            .for_new_key_fetch(create_fetch_initial_path(
                &dependency_graph.supergraph_schema,
                &dest_type.into(),
                new_context,
            )?),

        context: new_context,
        defer_context: updated_defer_context,
        context_to_condition_nodes: stack_item.context_to_condition_nodes.clone(),
    })
}

#[cfg_attr(feature = "snapshot_tracing", tracing::instrument(skip_all, level = "trace", fields(label = operation_element.to_string())))]
fn compute_nodes_for_op_path_element<'a>(
    dependency_graph: &mut FetchDependencyGraph,
    stack_item: &ComputeNodesStackItem<'a>,
    child: &'a Arc<PathTreeChild<OpGraphPathTrigger, Option<EdgeIndex>>>,
    operation_element: &OpPathElement,
    created_nodes: &mut IndexSet<NodeIndex>,
    check_cancellation: &dyn Fn() -> Result<(), SingleFederationError>,
) -> Result<ComputeNodesStackItem<'a>, FederationError> {
    let Some(edge_id) = child.edge else {
        // A null edge means that the operation does nothing
        // but may contain directives to preserve.
        // If it does contains directives, we look for @defer in particular.
        // If we find it, this means that we should change our current node
        // to one for the defer in question.
        let (updated_operation, updated_defer_context) = extract_defer_from_operation(
            dependency_graph,
            operation_element,
            &stack_item.defer_context,
            &stack_item.node_path,
        )?;
        // We've now removed any @defer.
        // If the operation contains other directives or a non-trivial type condition,
        // we need to preserve it and so we add operation.
        // Otherwise, we just skip it as a minor optimization (it makes the subgraph query
        // slightly smaller and on complex queries, it might also deduplicate similar selections).
        return Ok(ComputeNodesStackItem {
            tree: &child.tree,
            node_id: stack_item.node_id,
            node_path: match updated_operation {
                Some(op) if !op.directives().is_empty() => {
                    stack_item.node_path.add(Arc::new(op))?
                }
                _ => stack_item.node_path.clone(),
            },
            context: stack_item.context,
            defer_context: updated_defer_context,
            context_to_condition_nodes: stack_item.context_to_condition_nodes.clone(),
        });
    };
    let (source_id, dest_id) = stack_item.tree.graph.edge_endpoints(edge_id)?;
    let source = stack_item.tree.graph.node_weight(source_id)?;
    let dest = stack_item.tree.graph.node_weight(dest_id)?;
    let edge = stack_item.tree.graph.edge_weight(edge_id)?;
    if source.source != dest.source {
        return Err(FederationError::internal(format!(
            "Collecting edge {edge_id:?} for {operation_element:?} \
                 should not change the underlying subgraph"
        )));
    }

    // We have a operation element, field or inline fragment.
    // We first check if it's been "tagged" to remember that __typename must be queried.
    // See the comment on the `optimize_sibling_typenames()` method to see why this exists.
    if let Some(sibling_typename) = operation_element.sibling_typename() {
        // We need to add the query __typename for the current type in the current node.
        let typename_field = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
            operation_element.schema(),
            &operation_element.parent_type_position(),
            sibling_typename.alias().cloned(),
        )));
        let typename_path = stack_item
            .node_path
            .path_in_node
            .with_pushed(typename_field.clone());
        let node =
            FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
        node.selection_set_mut().add_at_path(&typename_path, None)?;
        dependency_graph.defer_tracking.update_subselection(
            &DeferContext {
                path_to_defer_parent: Arc::new(
                    stack_item
                        .defer_context
                        .path_to_defer_parent
                        .with_pushed(typename_field),
                ),
                ..stack_item.defer_context.clone()
            },
            None,
        )?
    }
    let Ok((Some(updated_operation), updated_defer_context)) = extract_defer_from_operation(
        dependency_graph,
        operation_element,
        &stack_item.defer_context,
        &stack_item.node_path,
    ) else {
        return Err(FederationError::internal(format!(
            "Extracting @defer from {operation_element:?} should not have resulted in no operation"
        )));
    };
    let mut updated = ComputeNodesStackItem {
        tree: &child.tree,
        node_id: stack_item.node_id,
        node_path: stack_item.node_path.clone(),
        context: stack_item.context,
        defer_context: updated_defer_context,
        context_to_condition_nodes: stack_item.context_to_condition_nodes.clone(),
    };
    if let Some(conditions) = &child.conditions {
        // We have @requires or some other dependency to create nodes for.
        let conditions_node_data = handle_conditions_tree(
            dependency_graph,
            conditions,
            (stack_item.node_id, &stack_item.node_path),
            // If setting a context, add __typename to the site where we are retrieving context from
            // since the context rewrites path will start with a type condition.
            if child.matching_context_ids.is_some() {
                Some(edge_id)
            } else {
                None
            },
            &updated.defer_context,
            created_nodes,
            check_cancellation,
        )?;

        if let Some(matching_context_ids) = &child.matching_context_ids {
            let mut condition_nodes = vec![conditions_node_data.conditions_merge_node_id];
            condition_nodes.extend(&conditions_node_data.created_node_ids);
            let mut context_to_condition_nodes =
                stack_item.context_to_condition_nodes.deref().clone();
            for context in matching_context_ids {
                context_to_condition_nodes.insert(context.clone(), condition_nodes.clone());
            }
            updated.context_to_condition_nodes = Arc::new(context_to_condition_nodes);
        }

        if edge.conditions.is_some() {
            // This edge needs the conditions just fetched, to be provided via _entities (@requires
            // or fake interface object downcast). So we create the post-@requires group, adding the
            // subgraph jump (if it isn't optimized away).
            let (required_node_id, require_path) = create_post_requires_node(
                dependency_graph,
                edge_id,
                (stack_item.node_id, &stack_item.node_path),
                stack_item.context,
                conditions_node_data,
                created_nodes,
            )?;
            updated.node_id = required_node_id;
            updated.node_path = require_path;
        }
    }

    // If the edge uses context variables, every context used must be set in a different parent
    // node or else we need to create a new one.
    if let Some(arguments_to_context_usages) = &child.arguments_to_context_usages {
        let mut conditions_nodes: IndexSet<NodeIndex> = Default::default();
        let mut is_subgraph_jump_needed = false;
        for context_usage in arguments_to_context_usages.values() {
            let Some(context_nodes) = updated
                .context_to_condition_nodes
                .get(&context_usage.context_id)
            else {
                bail!(
                    "Could not find condition nodes for context {}",
                    context_usage.context_id
                );
            };
            conditions_nodes.extend(context_nodes);
            if context_nodes
                .first()
                .is_some_and(|node_id| *node_id == updated.node_id)
            {
                is_subgraph_jump_needed = true;
            }
        }
        if is_subgraph_jump_needed {
            if updated.node_id != stack_item.node_id {
                bail!("Node created by post-@requires handling shouldn't have set context already");
            }

            let source_type: CompositeTypeDefinitionPosition = source.type_.clone().try_into()?;
            let source_schema: ValidFederationSchema = dependency_graph
                .federated_query_graph
                .schema_by_source(&source.source)?
                .clone();
            let path_in_parent = &stack_item.node_path.path_in_node;
            // NOTE: We should re-examine defer-handling for path elements in this function in the
            // future to ensure they're working as intended.
            let new_node_id = dependency_graph.get_or_create_key_node(
                &source.source,
                &stack_item.node_path.response_path,
                &source_type,
                ParentRelation {
                    parent_node_id: stack_item.node_id,
                    path_in_parent: Some(Arc::clone(path_in_parent)),
                },
                &conditions_nodes,
                None,
            )?;
            created_nodes.insert(new_node_id);
            updated.node_id = new_node_id;
            updated.node_path = stack_item
                .node_path
                .for_new_key_fetch(create_fetch_initial_path(
                    &dependency_graph.supergraph_schema,
                    &source_type,
                    stack_item.context,
                )?);

            let Some(key_condition) = stack_item
                .tree
                .graph
                .get_locally_satisfiable_key(source_id)?
            else {
                bail!(
                    "can_satisfy_conditions() validation should have required a key to be present for edge {}",
                    edge,
                )
            };
            let mut key_inputs =
                SelectionSet::for_composite_type(source_schema.clone(), source_type.clone());
            key_inputs.add_selection_set(&key_condition)?;
            let node = FetchDependencyGraph::node_weight_mut(
                &mut dependency_graph.graph,
                stack_item.node_id,
            )?;
            node.selection_set
                .add_at_path(path_in_parent, Some(&Arc::new(key_inputs)))?;

            let Ok(input_type) = CompositeTypeDefinitionPosition::try_from(
                dependency_graph
                    .supergraph_schema
                    .get_type(source_type.type_name())?,
            ) else {
                bail!(
                    "Type {} should exist in the supergraph and be a composite type",
                    source_type.type_name()
                );
            };
            let mut input_selection_set = SelectionSet::for_composite_type(
                dependency_graph.supergraph_schema.clone(),
                input_type.clone(),
            );
            input_selection_set.add_selection_set(&key_condition)?;
            let inputs = wrap_input_selections(
                &dependency_graph.supergraph_schema,
                &input_type,
                input_selection_set,
                stack_item.context,
            );
            let input_rewrites = compute_input_rewrites_on_key_fetch(
                source_type.type_name(),
                &source_type,
                &source_schema,
            )?;
            let updated_node = FetchDependencyGraph::node_weight_mut(
                &mut dependency_graph.graph,
                updated.node_id,
            )?;
            updated_node.add_inputs(&inputs, input_rewrites.into_iter().flatten())?;

            // Add the condition nodes as parent nodes.
            for parent_node_id in conditions_nodes {
                dependency_graph.add_parent(
                    updated.node_id,
                    ParentRelation {
                        parent_node_id,
                        path_in_parent: None,
                    },
                );
            }

            // Add context renamers.
            for context_entry in arguments_to_context_usages.values() {
                let updated_node = FetchDependencyGraph::node_weight_mut(
                    &mut dependency_graph.graph,
                    updated.node_id,
                )?;
                updated_node.add_input_context(
                    context_entry.context_id.clone(),
                    context_entry.subgraph_argument_type.clone(),
                )?;
                updated_node.add_context_renamers_for_selection_set(
                    Some(&context_entry.selection_set),
                    context_entry.relative_path.clone(),
                    context_entry.context_id.clone(),
                )?;
            }
        } else {
            // In this case we can just continue with the current node, but we need to add the
            // condition nodes as parents and the context renamers.
            for parent_node_id in conditions_nodes {
                dependency_graph.add_parent(
                    updated.node_id,
                    ParentRelation {
                        parent_node_id,
                        path_in_parent: None,
                    },
                );
            }
            let num_fields = updated
                .node_path
                .path_in_node
                .iter()
                .filter(|e| matches!((**e).deref(), OpPathElement::Field(_)))
                .count();
            for context_entry in arguments_to_context_usages.values() {
                let new_relative_path = &context_entry.relative_path
                    [..(context_entry.relative_path.len() - num_fields)];
                let updated_node = FetchDependencyGraph::node_weight_mut(
                    &mut dependency_graph.graph,
                    updated.node_id,
                )?;
                updated_node.add_input_context(
                    context_entry.context_id.clone(),
                    context_entry.subgraph_argument_type.clone(),
                )?;
                updated_node.add_context_renamers_for_selection_set(
                    Some(&context_entry.selection_set),
                    new_relative_path.to_vec(),
                    context_entry.context_id.clone(),
                )?;
            }
        }
    }

    if let OpPathElement::Field(field) = &updated_operation
        && *field.name() == TYPENAME_FIELD
    {
        // Because of the optimization done in `QueryPlanner.optimizeSiblingTypenames`,
        // we will rarely get an explicit `__typename` edge here.
        // But one case where it can happen is where an @interfaceObject was involved,
        // and we had to force jumping to another subgraph for getting the "true" `__typename`.
        // However, this case can sometimes lead to fetch dependency node
        // that only exists for that `__typename` resolution and that "looks" useless.
        // That is, we could have a fetch dependency node that looks like:
        // ```
        //   Fetch(service: "Subgraph2") {
        //     {
        //       ... on I {
        //         __typename
        //         id
        //       }
        //     } =>
        //     {
        //       ... on I {
        //         __typename
        //       }
        //     }
        //   }
        // ```
        // but the trick is that the `__typename` in the input
        // will be the name of the interface itself (`I` in this case)
        // but the one return after the fetch will the name of the actual implementation
        // (some implementation of `I`).
        // *But* we later have optimizations that would remove such a node,
        // on the node that the output is included in the input,
        // which is in general the right thing to do
        // (and genuinely ensure that some useless nodes created when handling
        // complex @require gets eliminated).
        // So we "protect" the node in this case to ensure
        // that later optimization doesn't kick in in this case.
        let updated_node =
            FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, updated.node_id)?;
        updated_node.must_preserve_selection_set = true
    }
    if let QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. } = &edge.transition {
        // We shouldn't add the operation "as is" as it's a down-cast but we're "faking it".
        // However, if the operation has directives, we should preserve that.
        let OpPathElement::InlineFragment(inline) = updated_operation else {
            return Err(FederationError::internal(format!(
                "Unexpected operation {updated_operation} for edge {edge}"
            )));
        };
        if !inline.directives.is_empty() {
            // We want to keep the directives, but we clear the condition
            // since it's to a type that doesn't exists in the subgraph we're currently in.
            updated.node_path = updated
                .node_path
                .add(Arc::new(inline.with_updated_type_condition(None).into()))?;
        }
    } else {
        updated.node_path = updated.node_path.add(Arc::new(updated_operation))?;
    }
    Ok(updated)
}

/// A helper function to wrap the `initial` value with nested conditions from `context`.
fn wrap_selection_with_type_and_conditions<T>(
    supergraph_schema: &ValidFederationSchema,
    wrapping_type: &CompositeTypeDefinitionPosition,
    context: &OpGraphPathContext,
    initial: T,
    mut wrap_in_fragment: impl FnMut(InlineFragment, T) -> T,
) -> T {
    // PORT_NOTE: `unwrap` is used below, but the JS version asserts in `FragmentElement`'s constructor
    // as well. However, there was a comment that we should add some validation, which is restated below.
    // TODO: remove the `unwrap` with proper error handling, and ensure we have some intersection
    // between the wrapping_type type and the new type condition.
    let type_condition: CompositeTypeDefinitionPosition = supergraph_schema
        .get_type(wrapping_type.type_name())
        .unwrap()
        .try_into()
        .unwrap();

    if context.is_empty() {
        // PORT_NOTE: JS code looks for type condition in the wrapping type's schema based on
        // the name of wrapping type. Not sure why.
        return wrap_in_fragment(
            InlineFragment {
                schema: supergraph_schema.clone(),
                parent_type_position: wrapping_type.clone(),
                type_condition_position: Some(type_condition),
                directives: Default::default(), // None
                selection_id: SelectionId::new(),
            },
            initial,
        );
    }

    // We wrap type-casts around `initial` value along with @include/@skip directive.
    // Note that we use the same type condition on all nested fragments. However,
    // except for the first one, we could well also use fragments with no type condition.
    // The reason we do the former is mostly to preserve the older behavior, but the latter
    // would technically produce slightly smaller query plans.
    // TODO: Next major revision may consider changing this as stated above.
    context.iter().fold(initial, |acc, cond| {
        let directive = Directive {
            name: cond.kind.name(),
            arguments: vec![
                Argument {
                    name: name!("if"),
                    value: cond.value.clone().into(),
                }
                .into(),
            ],
        };
        wrap_in_fragment(
            InlineFragment {
                schema: supergraph_schema.clone(),
                parent_type_position: wrapping_type.clone(),
                type_condition_position: Some(type_condition.clone()),
                directives: [directive].into_iter().collect(),
                selection_id: SelectionId::new(),
            },
            acc,
        )
    })
}

fn wrap_input_selections(
    supergraph_schema: &ValidFederationSchema,
    wrapping_type: &CompositeTypeDefinitionPosition,
    selections: SelectionSet,
    context: &OpGraphPathContext,
) -> SelectionSet {
    wrap_selection_with_type_and_conditions(
        supergraph_schema,
        wrapping_type,
        context,
        selections,
        |fragment, sub_selections| {
            /* creates a new selection set of the form:
               {
                   ... on <fragment's parent type> {
                       <sub_selections>
                   }
               }
            */
            let parent_type_position = fragment.parent_type_position.clone();
            let selection = InlineFragmentSelection::new(fragment, sub_selections);
            SelectionSet::from_selection(parent_type_position, selection.into())
        },
    )
}

fn create_fetch_initial_path(
    supergraph_schema: &ValidFederationSchema,
    dest_type: &CompositeTypeDefinitionPosition,
    context: &OpGraphPathContext,
) -> Result<Arc<OpPath>, FederationError> {
    // We make sure that all `OperationPath` are based on the supergraph as `OperationPath` is
    // really about path on the input query/overall supergraph data (most other places already do
    // this as the elements added to the operation path are from the input query, but this is
    // an exception when we create an element from an type that may/usually will not be from the
    // supergraph). Doing this make sure we can rely on things like checking subtyping between
    // the types of a given path.
    let rebased_type: CompositeTypeDefinitionPosition = supergraph_schema
        .get_type(dest_type.type_name())?
        .try_into()?;
    Ok(Arc::new(wrap_selection_with_type_and_conditions(
        supergraph_schema,
        &rebased_type,
        context,
        Default::default(),
        |fragment, sub_path| {
            // Return an OpPath of the form: [<fragment>, ...<sub_path>]
            let front = vec![Arc::new(fragment.into())];
            OpPath(front.into_iter().chain(sub_path.0).collect())
        },
    )))
}

fn compute_input_rewrites_on_key_fetch(
    input_type_name: &Name,
    dest_type: &CompositeTypeDefinitionPosition,
    dest_schema: &ValidFederationSchema,
) -> Result<Option<Vec<Arc<FetchDataRewrite>>>, FederationError> {
    // When we send a fetch to a subgraph, the inputs __typename must essentially match `dest_type`
    // so the proper __resolveReference is called. If `dest_type` is a "normal" object type, that's
    // going to be fine by default, but if `dest_type` is an interface in the supergraph (meaning
    // that it is either an interface or an interface object), then the underlying object might
    // have a __typename that is the concrete implementation type of the object, and we need to
    // rewrite it.
    if dest_type.is_interface_type()
        || dest_schema.is_interface_object_type(dest_type.clone().into())?
    {
        // rewrite path: [ ... on <input_type_name>, __typename ]
        let type_cond = FetchDataPathElement::TypenameEquals(input_type_name.clone());
        let typename_field_elem = FetchDataPathElement::Key(TYPENAME_FIELD, Default::default());
        let rewrite = FetchDataRewrite::ValueSetter(FetchDataValueSetter {
            path: vec![type_cond, typename_field_elem],
            set_value_to: dest_type.type_name().to_string().into(),
        });
        Ok(Some(vec![Arc::new(rewrite)]))
    } else {
        Ok(None)
    }
}

/// Returns an updated pair of (`operation`, `defer_context`) after the `defer` directive removed.
/// - The updated operation can be `None`, if operation is no longer necessary.
fn extract_defer_from_operation(
    dependency_graph: &mut FetchDependencyGraph,
    operation_element: &OpPathElement,
    defer_context: &DeferContext,
    node_path: &FetchDependencyGraphNodePath,
) -> Result<(Option<OpPathElement>, DeferContext), FederationError> {
    let defer_args = operation_element.defer_directive_args();
    let Some(defer_args) = defer_args else {
        let updated_path_to_defer_parent = defer_context
            .path_to_defer_parent
            .with_pushed(operation_element.clone().into());
        let updated_context = DeferContext {
            path_to_defer_parent: updated_path_to_defer_parent.into(),
            // Following fields are identical to those of `defer_context`.
            current_defer_ref: defer_context.current_defer_ref.clone(),
            active_defer_ref: defer_context.active_defer_ref.clone(),
            is_part_of_query: defer_context.is_part_of_query,
        };
        return Ok((Some(operation_element.clone()), updated_context));
    };

    // PORT_NOTE: The original TypeScript code has an assertion here.
    let updated_defer_ref = defer_args
        .label
        .as_ref()
        .ok_or_else(|| FederationError::internal("All defers should have a label at this point"))?;
    let updated_operation_element = operation_element.without_defer();
    let updated_path_to_defer_parent = match updated_operation_element {
        None => Default::default(), // empty OpPath
        Some(ref updated_operation) => OpPath(vec![Arc::new(updated_operation.clone())]),
    };

    dependency_graph.defer_tracking.register_defer(
        defer_context,
        &defer_args,
        node_path.clone(),
        operation_element.parent_type_position(),
    )?;

    let updated_context = DeferContext {
        current_defer_ref: Some(updated_defer_ref.into()),
        path_to_defer_parent: updated_path_to_defer_parent.into(),
        // Following fields are identical to those of `defer_context`.
        active_defer_ref: defer_context.active_defer_ref.clone(),
        is_part_of_query: defer_context.is_part_of_query,
    };
    Ok((updated_operation_element, updated_context))
}

struct ConditionsNodeData {
    conditions_merge_node_id: NodeIndex,
    path_in_conditions_merge_node_id: Option<Arc<OpPath>>,
    created_node_ids: Vec<NodeIndex>,
    is_fully_local_requires: bool,
}

/// Computes nodes for conditions imposed by @requires, @fromContext, and @interfaceObject, merging
/// them into ancestors as an optimization if possible. This does not modify the current node to
/// use the condition data as input, nor does it create parent-child relationships with created
/// nodes and the current node.
fn handle_conditions_tree(
    dependency_graph: &mut FetchDependencyGraph,
    conditions: &OpPathTree,
    (fetch_node_id, fetch_node_path): (NodeIndex, &FetchDependencyGraphNodePath),
    query_graph_edge_id_if_typename_needed: Option<EdgeIndex>,
    defer_context: &DeferContext,
    created_nodes: &mut IndexSet<NodeIndex>,
    check_cancellation: &dyn Fn() -> Result<(), SingleFederationError>,
) -> Result<ConditionsNodeData, FederationError> {
    // In many cases, we can optimize conditions by merging the fields into previously existing
    // nodes. However, we only do this when the current node has only a single parent (it's hard to
    // reason about it otherwise). But the current node could have multiple parents due to the graph
    // lacking minimality, and we don't want that to needlessly prevent us from this optimization.
    // So we do a graph reduction first (which effectively just eliminates unnecessary edges). To
    // illustrate, we could be in a case like:
    //    1
    //  /  \
    // 0 --- 2
    // with current node 2. And while the node currently has 2 parents, the `reduce` step will
    // ensure the edge `0 --- 2` is removed (since the dependency of 2 on 0 is already provided
    // transitively through 1).
    dependency_graph.reduce();

    // In general, we should do like for a key edge, and create a new node _for the current
    // subgraph_ that depends on the created nodes and have the created nodes depend on the current
    // one. However, we can be more efficient in general (and this is expected by the user) because
    // condition fields will usually come just after a key edge (at the top of a fetch node).
    // In that case (when the path is only type conditions), we can put the created nodes directly
    // as dependencies of the current node, avoiding creation of a new one. Additionally, if the
    // node we're coming from is our "direct parent", we can merge it to said direct parent (which
    // effectively means that the parent node will collect subgraph-local condition fields before
    // taking the edge to our current node).
    let copied_node_id_and_parent =
        match iter_into_single_item(dependency_graph.parents_relations_of(fetch_node_id)) {
            Some(parent) if fetch_node_path.path_in_node.has_only_fragments() => {
                // Since we may want the condition fields in this case to be added into an earlier
                // node, we create a copy of the current node (with only the inputs), and the
                // condition fields will be added to this node instead of the current one.
                let fetch_node = dependency_graph.node_weight(fetch_node_id)?;
                let subgraph_name = fetch_node.subgraph_name.clone();
                let Some(merge_at) = fetch_node.merge_at.clone() else {
                    bail!(
                        "Fetch node {} merge_at_path is required but was missing",
                        fetch_node_id.index()
                    );
                };
                let defer_ref = fetch_node.defer_ref.clone();
                let copied_node_id =
                    dependency_graph.new_key_node(&subgraph_name, merge_at, defer_ref)?;
                dependency_graph.add_parent(copied_node_id, parent.clone());
                dependency_graph.copy_inputs(copied_node_id, fetch_node_id)?;
                Some((copied_node_id, parent))
            }
            _ => None,
        };

    let condition_node_id = match &copied_node_id_and_parent {
        Some((copied_node_id, _)) => *copied_node_id,
        None => fetch_node_id,
    };
    if let Some(query_graph_edge_id) = query_graph_edge_id_if_typename_needed {
        let head = dependency_graph
            .federated_query_graph
            .edge_head_weight(query_graph_edge_id)?;
        let head_type: CompositeTypeDefinitionPosition = head.type_.clone().try_into()?;
        let head_schema = dependency_graph
            .federated_query_graph
            .schema_by_source(&head.source)?
            .clone();
        let typename_field = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
            &head_schema,
            &head_type,
            None,
        )));
        let typename_path = fetch_node_path.path_in_node.with_pushed(typename_field);
        let condition_node =
            FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, condition_node_id)?;
        condition_node
            .selection_set_mut()
            .add_at_path(&typename_path, None)?;
    }

    // Compute the node changes/additions introduced by the conditions path tree, using either the
    // current node or the copy of the current node if we expect to optimize.
    let newly_created_node_ids = compute_nodes_for_tree(
        dependency_graph,
        conditions,
        condition_node_id,
        fetch_node_path.clone(),
        defer_context.for_conditions(),
        &OpGraphPathContext::default(),
        check_cancellation,
    )?;

    if newly_created_node_ids.is_empty() {
        // All conditions were local. If we copied the node expecting to optimize, just merge it
        // back into the current node (we didn't need it) and continue.
        //
        // NOTE: This behavior is largely to maintain backwards compatibility with @requires. For
        // @fromContext, it still may be useful to merge the conditions into the parent if possible.
        // but we leave this optimization for later.
        if let Some((copied_node_id, _)) = copied_node_id_and_parent {
            if !dependency_graph.can_merge_sibling_in(fetch_node_id, copied_node_id)? {
                bail!(
                    "We should be able to merge {} into {} by construction",
                    copied_node_id.index(),
                    fetch_node_id.index()
                );
            }
            dependency_graph.merge_sibling_in(fetch_node_id, copied_node_id)?;
        }
        return Ok(ConditionsNodeData {
            conditions_merge_node_id: fetch_node_id,
            path_in_conditions_merge_node_id: Some(Arc::new(Default::default())),
            created_node_ids: vec![],
            is_fully_local_requires: true,
        });
    }

    if let Some((copied_node_id, parent)) = copied_node_id_and_parent {
        // We know the conditions depend on at least one created node. We do want to know, however,
        // if any of the condition fields was fetched from our copied node. If not, then this means
        // that the created nodes don't really depend on the current node and can be dependencies
        // of the parent (or even merged into the parent).
        //
        // So we want to know if anything in the copied node's selections cannot be fetched directly
        // from the parent. For that, we first remove any of the copied node's inputs from its
        // selections: in most cases, the copied node will just contain the key needed to jump back
        // to its parent, and those would usually be the same as its inputs. And since by definition
        // we know copied node's inputs are already fetched, we know they are not things that we
        // need. Then, we check if what remains (often empty) can be directly fetched from the
        // parent. If it can, then we can just merge the copied node into that parent. Otherwise, we
        // will have to "keep it".
        //
        // NOTE: We have explicitly copied the current node without its selections, so the current
        // node's fields should not pollute this check on the copied node.
        dependency_graph.remove_inputs_from_selection(copied_node_id)?;
        let copied_node_is_unneeded = dependency_graph.is_node_unneeded(copied_node_id, &parent)?;
        let mut unmerged_node_ids: Vec<NodeIndex> = Vec::new();
        if copied_node_is_unneeded {
            // We've removed the copied node's inputs from its own selections, and confirmed the
            // remaining fields can be fetched from the parent. As an optimization, we now merge it
            // into the parent, thus "rooting" the copied node's children to that parent. Note that
            // the copied node's selections are often empty after removing inputs, so merging it
            // into the parent is usually a no-op from that POV, except maybe for requesting
            // a few additional `__typename`s we didn't before.
            dependency_graph.merge_child_in(parent.parent_node_id, copied_node_id)?;

            // Now, all created nodes are going to be descendants of the parent node. But some of
            // them may actually be mergeable into it.
            for created_node_id in newly_created_node_ids {
                // Note that `created_node_id` may not be a direct child of `parent_node_id`, but
                // `can_merge_child_in()` just returns `false` in that case, yielding the behavior
                // we want (not trying to merge it in).
                if dependency_graph.can_merge_child_in(parent.parent_node_id, created_node_id)? {
                    dependency_graph.merge_child_in(parent.parent_node_id, created_node_id)?;
                } else {
                    unmerged_node_ids.push(created_node_id);

                    // `created_node_id` cannot be merged into `parent_node_id`, which may typically
                    // be because they aren't to the same subgraph. However, while `created_node_id`
                    // currently depends on `parent_node_id` (directly or indirectly), that
                    // dependency just comes from the fact that `parent_node_id` is the parent of
                    // the node whose conditions we're dealing with. And in practice, it could well
                    // be that some of the fetches needed for those conditions don't really depend
                    // on anything that the parent fetches and could be done in parallel with it. If
                    // we detect that this is the case for `created_node_id`, we can move it "up the
                    // chain of dependencies".
                    let mut current_parent = parent.clone();
                    while dependency_graph.is_child_of_with_artificial_dependency(
                        created_node_id,
                        current_parent.parent_node_id,
                    )? {
                        dependency_graph
                            .remove_child_edge(current_parent.parent_node_id, created_node_id);

                        let grand_parents: Vec<ParentRelation> = dependency_graph
                            .parents_relations_of(current_parent.parent_node_id)
                            .collect();
                        if grand_parents.is_empty() {
                            bail!(
                                "Fetch node {} is not top-level, so it should have parents",
                                current_parent.parent_node_id.index()
                            );
                        }
                        for grand_parent_relation in &grand_parents {
                            dependency_graph.add_parent(
                                created_node_id,
                                ParentRelation {
                                    parent_node_id: grand_parent_relation.parent_node_id,
                                    path_in_parent: concat_paths_in_parents(
                                        &grand_parent_relation.path_in_parent,
                                        &current_parent.path_in_parent,
                                    ),
                                },
                            )
                        }
                        // If we have more than 1 "grand parent", let's stop there as it would get more complicated
                        // and that's probably not needed. Otherwise, we can check if `created_node_id` may be able to move even
                        // further up.
                        if grand_parents.len() == 1 {
                            current_parent = grand_parents[0].clone();
                        } else {
                            break;
                        }
                    }
                }
            }
        } else {
            // We cannot merge the copied node into the parent because it fetches some conditions
            // fields that can't be fetched from the parent. We bail on this specific optimization,
            // and accordingly merge the copied node back to the original current node.
            if !dependency_graph.can_merge_sibling_in(fetch_node_id, copied_node_id)? {
                bail!(
                    "We should be able to merge {} into {} by construction",
                    copied_node_id.index(),
                    fetch_node_id.index()
                );
            };
            dependency_graph.merge_sibling_in(fetch_node_id, copied_node_id)?;

            // The created nodes depend on the current node, and the dependency cannot be moved to
            // the parent in this case. However, we might still be able to merge some created nodes
            // directly into the parent. But for this to be true, we should essentially make sure
            // that the dependency on the current node is not a "true" dependency. That is, if a
            // created node's inputs are the same as the current node's inputs (and said created
            // node is the same subgraph as the parent of the current node), then it means we depend
            // only on values that are already fetched by the parent and/or its ancestors, and
            // can merge that created node into the parent.
            if parent.path_in_parent.is_some() {
                for created_node_id in newly_created_node_ids {
                    if dependency_graph.can_merge_grand_child_in(
                        parent.parent_node_id,
                        fetch_node_id,
                        created_node_id,
                    )? {
                        dependency_graph
                            .merge_grand_child_in(parent.parent_node_id, created_node_id)?;
                    } else {
                        unmerged_node_ids.push(created_node_id);
                    }
                }
            } else {
                // Without a path into the parent, we cannot even evaluate the "merge into the
                // grand parent" optimization, so none of the created nodes can be merged. They
                // must still all be reported as created: they fetch the condition fields that the
                // post-@requires node depends on, and dropping them here would lose that
                // dependency and let the @requires be resolved before its conditions are fetched.
                unmerged_node_ids.extend(newly_created_node_ids);
            }
        }

        created_nodes.extend(unmerged_node_ids.clone());
        Ok(ConditionsNodeData {
            conditions_merge_node_id: if copied_node_is_unneeded {
                parent.parent_node_id
            } else {
                fetch_node_id
            },
            path_in_conditions_merge_node_id: if copied_node_is_unneeded {
                parent.path_in_parent
            } else {
                Some(Arc::new(Default::default()))
            },
            created_node_ids: unmerged_node_ids,
            is_fully_local_requires: false,
        })
    } else {
        // We're in the somewhat simpler case where the conditions are queried somewhere in the
        // middle of a subgraph fetch (so, not just after having jumped to that subgraph), or
        // there's more than one parent. In that case, there isn't much optimisation we can easily
        // do, so we leave the nodes as-is.
        created_nodes.extend(newly_created_node_ids.clone());
        Ok(ConditionsNodeData {
            conditions_merge_node_id: fetch_node_id,
            path_in_conditions_merge_node_id: Some(Arc::new(Default::default())),
            created_node_ids: newly_created_node_ids.into_iter().collect(),
            is_fully_local_requires: false,
        })
    }
}

/// Adds a @requires edge into the node at the given path, instead making a new node if optimization
/// cannot place that edge in the given node. This function assumes handle_conditions_tree() has
/// already been called, and accordingly takes its outputs.
fn create_post_requires_node(
    dependency_graph: &mut FetchDependencyGraph,
    query_graph_edge_id: EdgeIndex,
    (fetch_node_id, fetch_node_path): (NodeIndex, &FetchDependencyGraphNodePath),
    context: &OpGraphPathContext,
    conditions_node_data: ConditionsNodeData,
    created_nodes: &mut IndexSet<NodeIndex>,
) -> Result<(NodeIndex, FetchDependencyGraphNodePath), FederationError> {
    // @requires should be on an entity type, and we only support object types right now.
    let head = dependency_graph
        .federated_query_graph
        .edge_head_weight(query_graph_edge_id)?;
    let entity_type_schema = dependency_graph
        .federated_query_graph
        .schema_by_source(&head.source)?
        .clone();
    let QueryGraphNodeType::SchemaType(OutputTypeDefinitionPosition::Object(entity_type_position)) =
        head.type_.clone()
    else {
        bail!("@requires applied on non-entity object type");
    };

    // If all required fields could be fetched locally, we "continue" with the current node.
    if conditions_node_data.is_fully_local_requires {
        return Ok((fetch_node_id, fetch_node_path.clone()));
    }

    // NOTE: The code paths diverge below similar to handle_conditions_tree(), checking whether we
    // tried optimizing based on whether there's a single parent and whether the path in the node is
    // only type conditions. This is largely meant to just keep behavior the same as before and be
    // aligned with the JS query planner. This could change in the future though, to permit simpler
    // handling and further optimization. (There's also some arguably buggy behavior in this
    // function we ought to resolve in the future.)
    // Note that we also require a path into the parent: relocating the @requires into the parent
    // is only possible if we know where in the parent the current node applies. Without it there is
    // nothing to optimize, and we use the general handling below (which does not need that path).
    let parent_if_tried_optimizing =
        match iter_into_single_item(dependency_graph.parents_relations_of(fetch_node_id)) {
            Some(parent)
                if fetch_node_path.path_in_node.has_only_fragments()
                    && parent.path_in_parent.is_some() =>
            {
                Some(parent)
            }
            _ => None,
        };

    if let Some(parent) = parent_if_tried_optimizing {
        // If all created nodes were merged into ancestors, then those nodes' data are fetched
        // _before_ we get to the current node, so we "continue" with the current node.
        if conditions_node_data.created_node_ids.is_empty() {
            // We still need to add the required fields as inputs to the current node (but the node
            // should already have a key in its inputs, so we don't need to add that).
            let inputs = inputs_for_require(
                dependency_graph,
                entity_type_position,
                entity_type_schema,
                query_graph_edge_id,
                context,
                false,
            )?
            .0;
            let fetch_node =
                FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, fetch_node_id)?;
            fetch_node.add_inputs(&inputs, iter::empty())?;
            return Ok((fetch_node_id, fetch_node_path.clone()));
        }

        // If we get here, it means that @requires needs the fields from the created nodes (plus
        // potentially whatever has been merged before). So the node we should return, which is the
        // node where the "post-@requires" fields will be given as input, needs to a be a new node
        // that depends on all those created nodes.
        let fetch_node = dependency_graph.node_weight(fetch_node_id)?;
        let target_subgraph = fetch_node.subgraph_name.clone();
        let defer_ref = fetch_node.defer_ref.clone();
        let post_requires_node_id = dependency_graph.new_key_node(
            &target_subgraph,
            fetch_node_path.response_path.clone(),
            defer_ref,
        )?;
        // Note that the post-requires node cannot generally be merged into any of the created
        // nodes, and we accordingly don't provide a path in those created nodes.
        for created_node_id in &conditions_node_data.created_node_ids {
            dependency_graph.add_parent(
                post_requires_node_id,
                ParentRelation {
                    parent_node_id: *created_node_id,
                    path_in_parent: None,
                },
            );
        }
        // The post-requires node also needs to, in general, depend on the node that the @requires
        // conditions were merged into (either the current node or its parent).
        dependency_graph.add_parent(
            post_requires_node_id,
            ParentRelation {
                parent_node_id: conditions_node_data.conditions_merge_node_id,
                path_in_parent: conditions_node_data.path_in_conditions_merge_node_id,
            },
        );

        // NOTE(Sylvain): I'm not 100% sure about this assert in the sense that while I cannot think
        // of a case where `parent.path_in_parent` wouldn't exist, the code paths are complex enough
        // that I'm not able to prove this easily and could easily be missing something. That said,
        // we need the path here, so this will have to do for now, and if this ever breaks in
        // practice, we'll at least have an example to guide us toward improving/fixing the code.
        let Some(parent_path) = &parent.path_in_parent else {
            bail!(
                "Missing path_in_parent for @require on {} with group {} and parent {}",
                query_graph_edge_id.index(),
                fetch_node_id.index(),
                parent.parent_node_id.index()
            );
        };
        let path_for_parent = path_for_parent(
            dependency_graph,
            fetch_node_path,
            parent.parent_node_id,
            parent_path,
        )?;

        // The `post_requires_node_id` node needs a key. This can come from `fetch_node_id` (and
        // code in `GraphPath::can_satisfy_conditions()` guarantees such a locally satisfiable key
        // exists in `fetch_node_id`), but it can also potentially come from
        // `parent.parent_node_id`, and previous code had (wrongfully) always assumed it could.
        //
        // To keep this previous optimization, we now explicitly check whether the known locally
        // satisfiable key can be rebased in `parent.parent_node_id`, and we fall back to
        // `fetch_node_id` if it doesn't.
        let Some(key_condition) = dependency_graph
            .federated_query_graph
            .locally_satisfiable_key(query_graph_edge_id)?
        else {
            bail!(
                "Due to @requires, validation should have required a key to be present for {}",
                query_graph_edge_id.index()
            );
        };
        let parent_fetch_node = dependency_graph.node_weight(parent.parent_node_id)?;
        let type_at_path_for_parent = dependency_graph.type_at_path(
            &parent_fetch_node.selection_set.selection_set.type_position,
            &parent_fetch_node.selection_set.selection_set.schema,
            &path_for_parent.path_in_node,
        )?;
        let (require_node_path, pre_require_node_id) = if !key_condition.can_rebase_on(
            &type_at_path_for_parent,
            &parent_fetch_node.selection_set.selection_set.schema,
        )? {
            // It's possible we didn't add `fetch_node_id` as a parent previously, so we do so here
            // similarly to how `handle_conditions_tree()` specifies it.
            dependency_graph.add_parent(
                post_requires_node_id,
                ParentRelation {
                    parent_node_id: fetch_node_id,
                    path_in_parent: Some(Arc::new(Default::default())),
                },
            );
            (fetch_node_path, fetch_node_id)
        } else {
            (&path_for_parent, parent.parent_node_id)
        };

        add_post_require_inputs(
            dependency_graph,
            require_node_path,
            &entity_type_schema,
            entity_type_position.clone(),
            query_graph_edge_id,
            context,
            pre_require_node_id,
            post_requires_node_id,
        )?;
        created_nodes.insert(post_requires_node_id);
        let initial_fetch_path = create_fetch_initial_path(
            &dependency_graph.supergraph_schema,
            &entity_type_position.into(),
            context,
        )?;
        let new_path = fetch_node_path.for_new_key_fetch(initial_fetch_path);
        Ok((post_requires_node_id, new_path))
    } else {
        // We need to create a new node on the same subgraph as the current node, where we resume
        // fetching the field for which we handle the @requires _after_ we've dealt with any created
        // nodes. Note that during option generation, we already ensured a key exists, so the node
        // can resume properly.
        let fetch_node = dependency_graph.node_weight(fetch_node_id)?;
        let target_subgraph = fetch_node.subgraph_name.clone();
        let defer_ref = fetch_node.defer_ref.clone();
        let post_requires_node_id = dependency_graph.new_key_node(
            &target_subgraph,
            fetch_node_path.response_path.clone(),
            defer_ref,
        )?;
        let post_requires_node = dependency_graph.node_weight(post_requires_node_id)?;
        let merge_at = post_requires_node.merge_at.clone();
        let parent_type = post_requires_node.parent_type.clone();
        for created_node_id in &conditions_node_data.created_node_ids {
            let created_node = dependency_graph.node_weight(*created_node_id)?;
            // Usually, computing the path of the post-requires node in the created nodes is not
            // entirely trivial, but there is at least one relatively common case where the 2 nodes
            // we look at have (1) the same merge-at, and (2) the same parent type.
            //
            // In that case, we can basically infer those 2 nodes apply at the same "place" and so
            // the "path in parent" is empty.
            //
            // TODO(Sylvain): it should probably be possible to generalize this by checking the
            //     `merge_at` plus analyzing the selection, but that warrants some reflection...
            let new_path =
                if merge_at == created_node.merge_at && parent_type == created_node.parent_type {
                    Some(Arc::new(OpPath::default()))
                } else {
                    None
                };
            let new_parent_relation = ParentRelation {
                parent_node_id: *created_node_id,
                path_in_parent: new_path,
            };
            dependency_graph.add_parent(post_requires_node_id, new_parent_relation);
        }

        add_post_require_inputs(
            dependency_graph,
            fetch_node_path,
            &entity_type_schema,
            entity_type_position.clone(),
            query_graph_edge_id,
            context,
            fetch_node_id,
            post_requires_node_id,
        )?;
        created_nodes.insert(post_requires_node_id);
        let initial_fetch_path = create_fetch_initial_path(
            &dependency_graph.supergraph_schema,
            &entity_type_position.into(),
            context,
        )?;
        let new_path = fetch_node_path.for_new_key_fetch(initial_fetch_path);
        Ok((post_requires_node_id, new_path))
    }
}

impl DeferContext {
    /// Create a sub-context for use in resolving conditions inside an @defer block.
    fn for_conditions(&self) -> Self {
        let mut context = self.clone();
        context.is_part_of_query = false;
        context.current_defer_ref = self.active_defer_ref.clone();
        context
    }
}

fn inputs_for_require(
    fetch_dependency_graph: &mut FetchDependencyGraph,
    entity_type_position: ObjectTypeDefinitionPosition,
    entity_type_schema: ValidFederationSchema,
    query_graph_edge_id: EdgeIndex,
    context: &OpGraphPathContext,
    include_key_inputs: bool,
) -> Result<(SelectionSet, Option<SelectionSet>), FederationError> {
    // This method is actually called for to handle conditions of @requires, but also to fetch `__typename` in the
    // case of "fake downcast on an @interfaceObject". In that later case, once we fetched that `__typename`,
    // we want to wrap the input into the "downcasted" type, not the @interfaceObject one, so that we don't end
    // up querying some fields in the @interfaceObject subgraph for entities that we know won't match a type
    // condition of the query.
    let edge = fetch_dependency_graph
        .federated_query_graph
        .edge_weight(query_graph_edge_id)?;
    let (is_interface_object_down_cast, input_type_name) = match &edge.transition {
        QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { to_type_name, .. } => {
            (true, to_type_name.clone())
        }
        _ => (false, entity_type_position.type_name.clone()),
    };

    let Some(edge_conditions) = &edge.conditions else {
        return Err(FederationError::internal(
            "Missing edge conditions for @requires",
        ));
    };

    let input_type: CompositeTypeDefinitionPosition = fetch_dependency_graph
        .supergraph_schema
        .get_type(&input_type_name)?
        .try_into()
        .map_or_else(
            |_| {
                Err(FederationError::internal(format!(
                    "Type {} should exist in the supergraph and be a composite type",
                    input_type_name
                )))
            },
            Ok,
        )?;
    let mut full_selection_set = SelectionSet::for_composite_type(
        fetch_dependency_graph.supergraph_schema.clone(),
        input_type.clone(),
    );

    // JS PORT NOTE: we are manipulating selection sets in place which means we need to rebase new
    // elements before they can be merged. This is different from JS implementation which relied on
    // selection set "updates" to capture changes and apply them all at once (with rebasing) when
    // generating final selection set.
    full_selection_set.add_selection_set(edge_conditions)?;
    if include_key_inputs {
        let Some(key_condition) = fetch_dependency_graph
            .federated_query_graph
            .locally_satisfiable_key(query_graph_edge_id)?
        else {
            return Err(FederationError::internal(format!(
                "Due to @requires, validation should have required a key to be present for {}",
                query_graph_edge_id.index()
            )));
        };
        if is_interface_object_down_cast {
            // This means that conditions parents are on the @interfaceObject type, but we actually want to select only the
            // `input_type_name` implementation, the `merge_in` below will try to add fields from the interface to one of the
            // implementing type. Which `merge_in` usually let us do as that's safe, but because `key_condition` are on
            // the @interfaceObject subgraph, the type there is not an interface. To work around this, we "rebase" the
            // condition on the supergraph type (which is an interface) first, which lets the `mergeIn` work.
            let supergraph_intf_type: CompositeTypeDefinitionPosition = fetch_dependency_graph
                .supergraph_schema
                .get_type(&entity_type_position.type_name)?
                .try_into()?;
            if !supergraph_intf_type.is_interface_type() {
                return Err(FederationError::internal(format!(
                    "Type {} should be an interface in the supergraph",
                    entity_type_position.type_name
                )));
            };
            full_selection_set.add_selection_set(&key_condition)?;
        } else {
            full_selection_set.add_selection_set(&key_condition)?;
        }

        // Note that `key_inputs` are used to ensure those input are fetch on the original group, the one having `edge`. In
        // the case of an @interfaceObject downcast, that's the subgraph with said @interfaceObject, so in that case we
        // should just use `entity_type` (that @interfaceObject type), not input type which will be an implementation the
        // subgraph does not know in that particular case.
        let mut key_inputs =
            SelectionSet::for_composite_type(entity_type_schema, entity_type_position.into());
        key_inputs.add_selection_set(&key_condition)?;

        Ok((
            wrap_input_selections(
                &fetch_dependency_graph.supergraph_schema,
                &input_type,
                full_selection_set,
                context,
            ),
            Some(key_inputs),
        ))
    } else {
        Ok((
            wrap_input_selections(
                &fetch_dependency_graph.supergraph_schema,
                &input_type,
                full_selection_set,
                context,
            ),
            None,
        ))
    }
}

// Yes, many arguments, but this is an internal function with no obvious grouping
#[allow(clippy::too_many_arguments)]
fn add_post_require_inputs(
    dependency_graph: &mut FetchDependencyGraph,
    require_node_path: &FetchDependencyGraphNodePath,
    entity_type_schema: &ValidFederationSchema,
    entity_type_position: ObjectTypeDefinitionPosition,
    query_graph_edge_id: EdgeIndex,
    context: &OpGraphPathContext,
    pre_require_node_id: NodeIndex,
    post_require_node_id: NodeIndex,
) -> Result<(), FederationError> {
    let (inputs, key_inputs) = inputs_for_require(
        dependency_graph,
        entity_type_position.clone(),
        entity_type_schema.clone(),
        query_graph_edge_id,
        context,
        true,
    )?;
    // Note that `compute_input_rewrites_on_key_fetch` will return `None` in general, but if `entity_type_position` is an interface/interface object,
    // then we need those rewrites to ensure the underlying fetch is valid.
    let input_rewrites = compute_input_rewrites_on_key_fetch(
        &entity_type_position.type_name.clone(),
        &entity_type_position.into(),
        entity_type_schema,
    )?;
    let post_require_node =
        FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, post_require_node_id)?;
    post_require_node.add_inputs(&inputs, input_rewrites.into_iter().flatten())?;
    if let Some(key_inputs) = key_inputs {
        // It could be the key used to resume fetching after the @requires is already fetched in the original node, but we cannot
        // guarantee it, so we add it now (and if it was already selected, this is a no-op).
        let pre_require_node = FetchDependencyGraph::node_weight_mut(
            &mut dependency_graph.graph,
            pre_require_node_id,
        )?;
        pre_require_node
            .selection_set
            .add_at_path(&require_node_path.path_in_node, Some(&Arc::new(key_inputs)))?;
    }
    Ok(())
}

fn path_for_parent(
    dependency_graph: &FetchDependencyGraph,
    path: &FetchDependencyGraphNodePath,
    parent_node_id: NodeIndex,
    parent_path: &Arc<OpPath>,
) -> Result<FetchDependencyGraphNodePath, FederationError> {
    let parent_node = dependency_graph.node_weight(parent_node_id)?;
    let parent_schema = dependency_graph
        .federated_query_graph
        .schema_by_source(&parent_node.subgraph_name.clone())?;

    // The node referred by `path` may have types that do not exist in the node "parent", so we filter
    // out any type conditions on those. This typically happens jumping to a group that use an @interfaceObject
    // from a (parent) node that does not know the corresponding interface but has some of the type that
    // implements it (in the supergraph).
    let filtered_path = path.path_in_node.filter_on_schema(parent_schema);
    let final_path = concat_op_paths(parent_path.deref(), &filtered_path);
    Ok(FetchDependencyGraphNodePath {
        schema: dependency_graph.supergraph_schema.clone(),
        full_path: path.full_path.clone(),
        path_in_node: Arc::new(final_path),
        response_path: path.response_path.clone(),
        possible_types: path.possible_types.clone(),
        possible_types_after_last_field: path.possible_types_after_last_field.clone(),
        type_conditioned_fetching_enabled: path.type_conditioned_fetching_enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_condition_fetching_disabled() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
                type Query {
                    foo: Foo
                }
                interface Foo {
                    bar: Bar
                }
                interface Bar {
                    baz: String
                }
                type Foo_1 implements Foo {
                    bar: Bar_1
                    a: Int
                }
                type Foo_2 implements Foo {
                    bar: Bar_2
                    b: Int
                }
                type Bar_1 implements Bar {
                    baz: String
                    a: Int
                }
                type Bar_2 implements Bar {
                    baz: String
                    b: Int
                }
                type Bar_3 implements Bar {
                    baz: String
                }
            "#,
            "schema.graphql",
        )
        .unwrap();

        let valid_schema = ValidFederationSchema::new(schema).unwrap();

        let foo = object_field_element(&valid_schema, name!("Query"), name!("foo"));
        let frag = inline_fragment_element(&valid_schema, name!("Foo"), Some(name!("Foo_1")));
        let bar = object_field_element(&valid_schema, name!("Foo_1"), name!("bar"));
        let frag2 = inline_fragment_element(&valid_schema, name!("Bar"), Some(name!("Bar_1")));
        let baz = object_field_element(&valid_schema, name!("Bar_1"), name!("baz"));

        let query_root = valid_schema
            .get_type(&name!("Query"))
            .unwrap()
            .try_into()
            .unwrap();

        let path = FetchDependencyGraphNodePath::new(valid_schema, false, query_root).unwrap();

        let path = path.add(Arc::new(foo)).unwrap();
        let path = path.add(Arc::new(frag)).unwrap();
        let path = path.add(Arc::new(bar)).unwrap();
        let path = path.add(Arc::new(frag2)).unwrap();
        let path = path.add(Arc::new(baz)).unwrap();

        assert_eq!(".foo.bar.baz", &to_string(&path.response_path));
    }

    #[test]
    fn type_condition_fetching_enabled() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            r#"
                type Query {
                    foo: Foo
                }
                interface Foo {
                    bar: Bar
                }
                interface Bar {
                    baz: String
                }
                type Foo_1 implements Foo {
                    bar: Bar_1
                    a: Int
                }
                type Foo_2 implements Foo {
                    bar: Bar_2
                    b: Int
                }
                type Bar_1 implements Bar {
                    baz: String
                    a: Int
                }
                type Bar_2 implements Bar {
                    baz: String
                    b: Int
                }
                type Bar_3 implements Bar {
                    baz: String
                }
            "#,
            "schema.graphql",
        )
        .unwrap();

        let valid_schema = ValidFederationSchema::new(schema).unwrap();

        let foo = object_field_element(&valid_schema, name!("Query"), name!("foo"));
        let frag = inline_fragment_element(&valid_schema, name!("Foo"), Some(name!("Foo_1")));
        let bar = object_field_element(&valid_schema, name!("Foo_1"), name!("bar"));
        let frag2 = inline_fragment_element(&valid_schema, name!("Bar"), Some(name!("Bar_1")));
        let baz = object_field_element(&valid_schema, name!("Bar_1"), name!("baz"));

        let query_root = valid_schema
            .get_type(&name!("Query"))
            .unwrap()
            .try_into()
            .unwrap();

        let path = FetchDependencyGraphNodePath::new(valid_schema, true, query_root).unwrap();

        let path = path.add(Arc::new(foo)).unwrap();
        let path = path.add(Arc::new(frag)).unwrap();
        let path = path.add(Arc::new(bar)).unwrap();
        let path = path.add(Arc::new(frag2)).unwrap();
        let path = path.add(Arc::new(baz)).unwrap();

        assert_eq!(".|[Foo_1]foo.bar.baz", &to_string(&path.response_path));
    }

    fn object_field_element(
        schema: &ValidFederationSchema,
        object: Name,
        field: Name,
    ) -> OpPathElement {
        OpPathElement::Field(Field {
            schema: schema.clone(),
            field_position: ObjectTypeDefinitionPosition::new(object)
                .field(field)
                .into(),
            alias: None,
            arguments: Default::default(),
            directives: Default::default(),
            sibling_typename: None,
        })
    }

    fn inline_fragment_element(
        schema: &ValidFederationSchema,
        parent_type_name: Name,
        type_condition_name: Option<Name>,
    ) -> OpPathElement {
        let parent_type = schema
            .get_type(&parent_type_name)
            .unwrap()
            .try_into()
            .unwrap();
        let type_condition = type_condition_name
            .as_ref()
            .map(|n| schema.get_type(n).unwrap().try_into().unwrap());
        OpPathElement::InlineFragment(InlineFragment {
            schema: schema.clone(),
            parent_type_position: parent_type,
            type_condition_position: type_condition,
            directives: Default::default(),
            selection_id: SelectionId::new(),
        })
    }

    fn to_string(response_path: &[FetchDataPathElement]) -> String {
        format!(
            ".{}",
            response_path
                .iter()
                .map(|element| match element {
                    FetchDataPathElement::Key(name, conditions) => {
                        format!("{}{}", cond_to_string(conditions), name)
                    }
                    FetchDataPathElement::AnyIndex(conditions) => {
                        format!("{}{}", cond_to_string(conditions), "@")
                    }
                    FetchDataPathElement::TypenameEquals(_) => {
                        unimplemented!()
                    }
                    FetchDataPathElement::Parent => {
                        unimplemented!()
                    }
                })
                .join(".")
        )
    }

    fn cond_to_string(conditions: &Option<Vec<Name>>) -> String {
        if let Some(conditions) = conditions {
            return format!("|[{}]", conditions.iter().map(|n| n.to_string()).join(","));
        }
        Default::default()
    }

    // -----------------------------------------------------------------------
    // Regression tests for `reduced_defer_edges` sync.
    //
    // These build a `FetchDependencyGraph` directly from a small composed
    // supergraph and exercise the storage paths (record, retain, lookup) in
    // isolation. They surface the bugs the corresponding fixes target without
    // needing the planner to happen to produce the exact scenario.
    // -----------------------------------------------------------------------

    use crate::Supergraph;
    use crate::query_graph::build_federated_query_graph;

    /// Minimum supergraph that lets us build a `FetchDependencyGraph` and
    /// allocate nodes via `new_node` for two distinct subgraphs.
    const TEST_SUPERGRAPH_SDL: &str = include_str!(
        "../../tests/query_plan/supergraphs/\
         defer_test_handles_simple_defer_with_defer_enabled.graphql"
    );

    fn make_test_dep_graph() -> FetchDependencyGraph {
        let supergraph = Supergraph::new(TEST_SUPERGRAPH_SDL).unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let federated_query_graph = Arc::new(
            build_federated_query_graph(supergraph.schema.clone(), api_schema, None, None).unwrap(),
        );
        FetchDependencyGraph::new(
            supergraph.schema.clone(),
            federated_query_graph,
            None,
            Arc::new(FetchIdGenerator::new()),
        )
    }

    fn add_test_node(
        graph: &mut FetchDependencyGraph,
        subgraph_name: &str,
        defer_ref: Option<&str>,
    ) -> NodeIndex {
        let sg: Arc<str> = Arc::from(subgraph_name);
        let subgraph_schema = graph
            .federated_query_graph
            .schema_by_source(&sg)
            .unwrap()
            .clone();
        let parent_type: CompositeTypeDefinitionPosition = subgraph_schema
            .get_type(&name!("T"))
            .unwrap()
            .try_into()
            .unwrap();
        graph
            .new_node(
                sg,
                parent_type,
                false,
                SchemaRootDefinitionKind::Query,
                None,
                defer_ref.map(String::from),
            )
            .unwrap()
    }

    fn add_test_edge(graph: &mut FetchDependencyGraph, from: NodeIndex, to: NodeIndex) {
        graph
            .graph
            .add_edge(from, to, Arc::new(FetchDependencyGraphEdge { path: None }));
    }

    /// Regression for the gap in `remove_redundant_edges`: when a post-merge
    /// transitive reduction strips an edge that crosses a defer boundary, the
    /// edge must be recorded so the deferred block's dependency survives.
    /// Without `record_reduced_defer_edges` wired into `remove_redundant_edges`
    /// the recording silently doesn't happen, and `reduced_defer_edges` stays
    /// empty.
    #[test]
    fn remove_redundant_edges_records_defer_crossing_edges() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);
        // A → C is transitively reachable via A → B → C and crosses a defer
        // boundary (A is primary, C is deferred).
        add_test_edge(&mut graph, a, c);

        assert!(graph.reduced_defer_edges.is_empty());

        graph.remove_redundant_edges(a);

        assert_eq!(
            graph.reduced_defer_edges,
            vec![(a, c)],
            "expected the defer-crossing redundant edge to be recorded"
        );
    }

    /// Regression for the latent `node_weight(target)?` crash in
    /// `collect_reduced_defer_dependencies`: if a recorded target was removed
    /// from the graph between recording and use, the lookup must skip the
    /// entry rather than bubble `Node unexpectedly missing`.
    #[test]
    fn collect_reduced_defer_dependencies_skips_removed_target() {
        let mut graph = make_test_dep_graph();
        let s = add_test_node(&mut graph, "Subgraph1", None);
        let t = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        graph.reduced_defer_edges.push((s, t));

        // Bypass the sync wrappers (`remove_node`, `retain_nodes`) so the
        // stale entry survives — simulating a path that doesn't go through
        // them. The lookup must still be safe.
        graph.graph.remove_node(t);

        let mut deps = Vec::new();
        let result = graph.collect_reduced_defer_dependencies(s, &mut deps);
        assert!(
            result.is_ok(),
            "lookup must not crash on a stale target; got {result:?}"
        );
        assert!(deps.is_empty());
    }

    /// Regression for the missing lockstep cleanup in `retain_nodes`: when a
    /// node referenced by a recorded edge is dropped via the bulk retain API
    /// (used by `remove_empty_nodes`), the entry must be pruned in lockstep.
    /// Without the fix, the stale entry would survive in `reduced_defer_edges`
    /// (and, on the unguarded lookup path, surface as the
    /// `Node unexpectedly missing` internal error).
    ///
    /// Exercise both endpoints so each clause of the
    /// `predicate(&s) && predicate(&t)` filter is covered.
    #[test]
    fn retain_nodes_prunes_reduced_defer_edges_when_target_removed() {
        let mut graph = make_test_dep_graph();
        let s = add_test_node(&mut graph, "Subgraph1", None);
        let t = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        graph.reduced_defer_edges.push((s, t));

        graph.retain_nodes(|&n| n != t);

        assert!(
            graph.reduced_defer_edges.is_empty(),
            "expected the entry referencing the dropped target to be pruned, got {:?}",
            graph.reduced_defer_edges
        );
    }

    #[test]
    fn retain_nodes_prunes_reduced_defer_edges_when_source_removed() {
        let mut graph = make_test_dep_graph();
        let s = add_test_node(&mut graph, "Subgraph1", None);
        let t = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        graph.reduced_defer_edges.push((s, t));

        graph.retain_nodes(|&n| n != s);

        assert!(
            graph.reduced_defer_edges.is_empty(),
            "expected the entry referencing the dropped source to be pruned, got {:?}",
            graph.reduced_defer_edges
        );
    }

    // =========================================================================================
    // Sentinel 1 (proptest-graph-notes.md, "Graph PR 1"): FetchDependencyGraph mutation and
    // reduction model.
    //
    // `DagCommand` drives a `FetchDependencyGraph` through its own private mutation methods
    // (`new_node`, `add_parent`, `remove_node`, `retain_nodes`, `reduce`,
    // `remove_redundant_edges`), while `DagModel` performs the same mutation against a naive,
    // independently-written in-memory graph (plain BFS/DFS reachability, no Petgraph transitive
    // reduction). After every command, `audit_dag_matches_model` recomputes every fact the
    // production graph caches — the edge set, acyclicity, `is_reduced`/`is_optimized`, and the
    // `reduced_defer_edges` ledger — from the model's own state and compares it to production.
    //
    // Node identity is the production `NodeIndex` itself (`StableDiGraph` reuses freed indices,
    // so a node's `NodeIndex` is not proof of insertion order). A separate monotonic `rank` is
    // assigned at creation and used only to orient generated edges low-rank -> high-rank, which
    // guarantees every generated trace stays acyclic without ever needing to reject a command.
    // =========================================================================================

    use proptest::prelude::*;
    use proptest::proptest;
    use proptest::test_runner::TestCaseError;

    use crate::schema::field_set::parse_field_set;

    /// Only the two subgraphs the shared test fixture (`TEST_SUPERGRAPH_SDL`) defines.
    const DAG_TEST_SUBGRAPHS: [&str; 2] = ["Subgraph1", "Subgraph2"];

    /// Deliberate adversarial shapes from proptest-graph-notes.md's "Required adversarial
    /// shapes" list (the topology-relevant subset; the rest — mergeable siblings, nested
    /// `@requires`, interface/union restrictions, list-of-list paths — need selection/input
    /// payloads that are out of scope for this topology-only harness). Each shape is applied as
    /// an atomic unit using freshly-created nodes, so it composes with arbitrary surrounding
    /// commands regardless of where in the trace it lands.
    #[derive(Debug, Clone, Copy)]
    enum DagShape {
        /// A -> B -> C.
        Chain,
        /// A -> B -> C, plus a redundant direct A -> C.
        Shortcut,
        /// A -> B, A -> C, B -> D, C -> D: no edge here is individually redundant.
        Diamond,
        /// A diamond plus a redundant direct A -> D.
        DiamondWithShortcut,
        /// A -> M, B -> M, M -> X, M -> Y: multiple roots into a shared middle, fanning back out.
        BowTie,
        /// A bow tie plus a redundant direct A -> X.
        BowTieWithShortcut,
        /// A(primary) -> B(primary) -> C(deferred), plus a direct A -> C shortcut that crosses
        /// the defer boundary and must be reduced-defer-ledgered rather than just dropped.
        DeferCrossingShortcut,
        /// Remove an existing live node (if any), then add two fresh nodes and connect them —
        /// the new nodes are likely to reuse the freed `NodeIndex` slot.
        StableIndexHole,
    }

    fn dag_shape_strategy() -> impl Strategy<Value = DagShape> {
        prop_oneof![
            Just(DagShape::Chain),
            Just(DagShape::Shortcut),
            Just(DagShape::Diamond),
            Just(DagShape::DiamondWithShortcut),
            Just(DagShape::BowTie),
            Just(DagShape::BowTieWithShortcut),
            Just(DagShape::DeferCrossingShortcut),
            Just(DagShape::StableIndexHole),
        ]
    }

    #[derive(Debug, Clone, Copy)]
    enum DagCommand {
        AddNode {
            subgraph: u8,
            defer_group: Option<u8>,
        },
        AddParent {
            parent: u16,
            child: u16,
        },
        RemoveNode {
            node: u16,
        },
        RetainByMask {
            mask: u32,
        },
        Reduce,
        ReduceFrom {
            node: u16,
        },
        /// Deliberately construct one of the required adversarial shapes above, using freshly
        /// created nodes (`StableIndexHole` additionally consumes `victim` to pick a node to
        /// remove first).
        Shape {
            shape: DagShape,
            subgraph_seed: u8,
            defer_seed: u8,
            victim: u16,
        },
    }

    fn dag_defer_group_strategy() -> impl Strategy<Value = Option<u8>> {
        prop_oneof![
            3 => Just(None),
            7 => (0u8..4).prop_map(Some),
        ]
    }

    fn dag_command_strategy() -> impl Strategy<Value = DagCommand> {
        prop_oneof![
            2 => (any::<u8>(), dag_defer_group_strategy())
                .prop_map(|(subgraph, defer_group)| DagCommand::AddNode {
                    subgraph,
                    defer_group
                }),
            4 => (any::<u16>(), any::<u16>())
                .prop_map(|(parent, child)| DagCommand::AddParent { parent, child }),
            1 => any::<u16>().prop_map(|node| DagCommand::RemoveNode { node }),
            1 => any::<u32>().prop_map(|mask| DagCommand::RetainByMask { mask }),
            2 => Just(DagCommand::Reduce),
            1 => any::<u16>().prop_map(|node| DagCommand::ReduceFrom { node }),
            // Weighted so that, combined with the usual 0..40 trace length, the overwhelming
            // majority of generated traces contain at least one deliberate adversarial shape —
            // comfortably above the notes' "at least half" floor — while `AddNode`/`AddParent`
            // above still generate general, unstructured valid DAGs the rest of the time.
            6 => (dag_shape_strategy(), any::<u8>(), any::<u8>(), any::<u16>()).prop_map(
                |(shape, subgraph_seed, defer_seed, victim)| DagCommand::Shape {
                    shape,
                    subgraph_seed,
                    defer_seed,
                    victim,
                }
            ),
        ]
    }

    fn dag_command_trace_strategy() -> impl Strategy<Value = Vec<DagCommand>> {
        prop::collection::vec(dag_command_strategy(), 0..40)
    }

    /// Naive reference model for `FetchDependencyGraph`'s topology. Deliberately independent of
    /// every production algorithm: no `dag_transitive_reduction_closure`, no incremental
    /// bookkeeping, just plain graph traversal recomputed from scratch each time.
    #[derive(Default)]
    struct DagModel {
        live: IndexSet<NodeIndex>,
        rank: IndexMap<NodeIndex, u64>,
        defer_group: IndexMap<NodeIndex, Option<u8>>,
        edges: IndexSet<(NodeIndex, NodeIndex)>,
        /// Mirrors `FetchDependencyGraph::reduced_defer_edges`. Compared as a multiset: per the
        /// sprint decision log, internal duplicates are not (yet) known to be a contract
        /// violation, only the externally-collected dependency set is.
        ledger: Vec<(NodeIndex, NodeIndex)>,
        next_rank: u64,
        is_reduced: bool,
    }

    impl DagModel {
        fn sorted_live(&self) -> Vec<NodeIndex> {
            let mut nodes: Vec<NodeIndex> = self.live.iter().copied().collect();
            nodes.sort_by_key(|n| n.index());
            nodes
        }

        /// Choice-by-modulo selection of a currently-live node (see proptest-graph-notes.md).
        fn pick(&self, choice: u16) -> Option<NodeIndex> {
            let sorted = self.sorted_live();
            if sorted.is_empty() {
                None
            } else {
                Some(sorted[choice as usize % sorted.len()])
            }
        }

        fn children(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
            self.edges
                .iter()
                .filter(move |&&(source, _)| source == node)
                .map(|&(_, target)| target)
        }

        fn parents(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
            self.edges
                .iter()
                .filter(move |&&(_, target)| target == node)
                .map(|&(source, _)| source)
        }

        /// Nodes reachable from `start` via a path of length >= 1 (proper descendants).
        fn descendants(&self, start: NodeIndex) -> IndexSet<NodeIndex> {
            let mut visited = IndexSet::default();
            let mut stack: Vec<NodeIndex> = self.children(start).collect();
            while let Some(node) = stack.pop() {
                if visited.insert(node) {
                    stack.extend(self.children(node));
                }
            }
            visited
        }

        /// Whether `target` remains reachable from `source` using every edge except `excluded`.
        fn reachable_excluding(
            &self,
            source: NodeIndex,
            target: NodeIndex,
            excluded: (NodeIndex, NodeIndex),
        ) -> bool {
            let mut visited = IndexSet::default();
            let mut stack = vec![source];
            while let Some(node) = stack.pop() {
                for child in self.children(node) {
                    if (node, child) == excluded {
                        continue;
                    }
                    if child == target {
                        return true;
                    }
                    if visited.insert(child) {
                        stack.push(child);
                    }
                }
            }
            false
        }

        /// Global transitive reduction: an edge is redundant iff its endpoints remain connected
        /// without it. For a DAG this set is unique, so this is directly comparable to whatever
        /// edges `dag_transitive_reduction_closure` retains.
        fn globally_redundant_edges(&self) -> IndexSet<(NodeIndex, NodeIndex)> {
            self.edges
                .iter()
                .copied()
                .filter(|&(source, target)| {
                    self.reachable_excluding(source, target, (source, target))
                })
                .collect()
        }

        /// Local reduction restricted to edges leaving `node`, matching
        /// `FetchDependencyGraph::remove_redundant_edges`: an edge `node -> v` is redundant iff
        /// `v` is also reachable from `node` via some other direct child (a path of length >= 2).
        fn locally_redundant_edges_from(
            &self,
            node: NodeIndex,
        ) -> IndexSet<(NodeIndex, NodeIndex)> {
            let mut reachable_via_other_child = IndexSet::default();
            for child in self.children(node).collect::<Vec<_>>() {
                reachable_via_other_child.extend(self.descendants(child));
            }
            self.children(node)
                .filter(|target| reachable_via_other_child.contains(target))
                .map(|target| (node, target))
                .collect()
        }

        /// Mirrors `record_reduced_defer_edges`: only cross-defer-boundary edges are ledgered.
        fn record_ledger(&mut self, removed: &IndexSet<(NodeIndex, NodeIndex)>) {
            for &(source, target) in removed {
                if self.defer_group[&source] != self.defer_group[&target] {
                    self.ledger.push((source, target));
                }
            }
        }

        /// Mirrors the lockstep cleanup in `remove_node`/`retain_nodes`.
        fn drop_node(&mut self, node: NodeIndex) {
            self.live.shift_remove(&node);
            self.rank.shift_remove(&node);
            self.defer_group.shift_remove(&node);
            self.edges.retain(|&(s, t)| s != node && t != node);
            self.ledger.retain(|&(s, t)| s != node && t != node);
        }
    }

    /// Apply one command to both the production graph and the reference model. Every branch
    /// documents which production `on_modification`/flag behavior it is mirroring.
    fn apply_dag_command(
        graph: &mut FetchDependencyGraph,
        model: &mut DagModel,
        command: &DagCommand,
    ) {
        match *command {
            DagCommand::AddNode {
                subgraph,
                defer_group,
            } => {
                let subgraph_name =
                    DAG_TEST_SUBGRAPHS[subgraph as usize % DAG_TEST_SUBGRAPHS.len()];
                let defer_ref = defer_group.map(|g| format!("d{g}"));
                let node = add_test_node(graph, subgraph_name, defer_ref.as_deref());
                model.live.insert(node);
                model.rank.insert(node, model.next_rank);
                model.next_rank += 1;
                model.defer_group.insert(node, defer_group);
                // `new_node` unconditionally calls `on_modification`.
                model.is_reduced = false;
            }
            DagCommand::AddParent { parent, child } => {
                let (Some(a), Some(b)) = (model.pick(parent), model.pick(child)) else {
                    return;
                };
                if a == b {
                    // A self-relation: production's descendant assert would fire on this, so
                    // there is no valid interpretation other than skipping (a no-op trace entry).
                    return;
                }
                // Only ever add low-rank -> high-rank edges, so the trace can never build a
                // cycle even after `StableDiGraph` reuses a freed `NodeIndex`.
                let (lo, hi) = if model.rank[&a] < model.rank[&b] {
                    (a, b)
                } else {
                    (b, a)
                };
                if model.edges.contains(&(lo, hi)) {
                    // `add_parent` early-returns via `contains_edge` without calling
                    // `on_modification`.
                    return;
                }
                graph.add_parent(
                    hi,
                    ParentRelation {
                        parent_node_id: lo,
                        path_in_parent: None,
                    },
                );
                model.edges.insert((lo, hi));
                model.is_reduced = false;
            }
            DagCommand::RemoveNode { node } => {
                let Some(node) = model.pick(node) else {
                    return;
                };
                graph.remove_node(node);
                model.drop_node(node);
                // `remove_node` unconditionally calls `on_modification`.
                model.is_reduced = false;
            }
            DagCommand::RetainByMask { mask } => {
                let sorted = model.sorted_live();
                if sorted.is_empty() {
                    return;
                }
                // Bit `i` of `mask` decides whether to keep the `i`-th live node in index order;
                // beyond the mask's 32 bits, default to keeping (never shift by >= 32).
                let keep: IndexSet<NodeIndex> = sorted
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i >= 32 || (mask >> i) & 1 == 1)
                    .map(|(_, &n)| n)
                    .collect();
                graph.retain_nodes(|n| keep.contains(n));
                if keep.len() < sorted.len() {
                    for &node in &sorted {
                        if !keep.contains(&node) {
                            model.drop_node(node);
                        }
                    }
                    // `retain_nodes` only calls `on_modification` when the node count dropped.
                    model.is_reduced = false;
                }
            }
            DagCommand::Reduce => {
                if model.is_reduced {
                    graph.reduce(); // exercises the early-return; no state change expected.
                    return;
                }
                let removed = model.globally_redundant_edges();
                model.record_ledger(&removed);
                for edge in &removed {
                    model.edges.shift_remove(edge);
                }
                graph.reduce();
                model.is_reduced = true;
            }
            DagCommand::ReduceFrom { node } => {
                let Some(node) = model.pick(node) else {
                    return;
                };
                let removed = model.locally_redundant_edges_from(node);
                model.record_ledger(&removed);
                graph.remove_redundant_edges(node);
                for edge in &removed {
                    model.edges.shift_remove(edge);
                }
                if !removed.is_empty() {
                    // `remove_redundant_edges` only calls `on_modification` when it actually
                    // removed an edge.
                    model.is_reduced = false;
                }
            }
            DagCommand::Shape {
                shape,
                subgraph_seed,
                defer_seed,
                victim,
            } => apply_dag_shape(graph, model, shape, subgraph_seed, defer_seed, victim),
        }
    }

    /// Construct one adversarial shape using freshly created nodes, wired together directly by
    /// their concrete `NodeIndex` values (no modulo addressing needed, since the nodes are all
    /// created right here).
    fn apply_dag_shape(
        graph: &mut FetchDependencyGraph,
        model: &mut DagModel,
        shape: DagShape,
        subgraph_seed: u8,
        defer_seed: u8,
        victim: u16,
    ) {
        let subgraph_for = |i: u8| -> &'static str {
            DAG_TEST_SUBGRAPHS[(subgraph_seed.wrapping_add(i) as usize) % DAG_TEST_SUBGRAPHS.len()]
        };
        let add = |graph: &mut FetchDependencyGraph,
                   model: &mut DagModel,
                   i: u8,
                   defer_group: Option<u8>|
         -> NodeIndex {
            let defer_ref = defer_group.map(|g| format!("d{g}"));
            let node = add_test_node(graph, subgraph_for(i), defer_ref.as_deref());
            model.live.insert(node);
            model.rank.insert(node, model.next_rank);
            model.next_rank += 1;
            model.defer_group.insert(node, defer_group);
            node
        };
        let connect = |graph: &mut FetchDependencyGraph,
                       model: &mut DagModel,
                       parent: NodeIndex,
                       child: NodeIndex| {
            if model.edges.contains(&(parent, child)) {
                return;
            }
            graph.add_parent(
                child,
                ParentRelation {
                    parent_node_id: parent,
                    path_in_parent: None,
                },
            );
            model.edges.insert((parent, child));
        };

        match shape {
            DagShape::Chain => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let c = add(graph, model, 2, None);
                connect(graph, model, a, b);
                connect(graph, model, b, c);
            }
            DagShape::Shortcut => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let c = add(graph, model, 2, None);
                connect(graph, model, a, b);
                connect(graph, model, b, c);
                connect(graph, model, a, c);
            }
            DagShape::Diamond => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let c = add(graph, model, 2, None);
                let d = add(graph, model, 3, None);
                connect(graph, model, a, b);
                connect(graph, model, a, c);
                connect(graph, model, b, d);
                connect(graph, model, c, d);
            }
            DagShape::DiamondWithShortcut => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let c = add(graph, model, 2, None);
                let d = add(graph, model, 3, None);
                connect(graph, model, a, b);
                connect(graph, model, a, c);
                connect(graph, model, b, d);
                connect(graph, model, c, d);
                connect(graph, model, a, d);
            }
            DagShape::BowTie => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let m = add(graph, model, 2, None);
                let x = add(graph, model, 3, None);
                let y = add(graph, model, 4, None);
                connect(graph, model, a, m);
                connect(graph, model, b, m);
                connect(graph, model, m, x);
                connect(graph, model, m, y);
            }
            DagShape::BowTieWithShortcut => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let m = add(graph, model, 2, None);
                let x = add(graph, model, 3, None);
                let y = add(graph, model, 4, None);
                connect(graph, model, a, m);
                connect(graph, model, b, m);
                connect(graph, model, m, x);
                connect(graph, model, m, y);
                connect(graph, model, a, x);
            }
            DagShape::DeferCrossingShortcut => {
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                let c = add(graph, model, 2, Some(defer_seed % 4));
                connect(graph, model, a, b);
                connect(graph, model, b, c);
                connect(graph, model, a, c);
            }
            DagShape::StableIndexHole => {
                let Some(victim_node) = model.pick(victim) else {
                    return;
                };
                graph.remove_node(victim_node);
                model.drop_node(victim_node);
                let a = add(graph, model, 0, None);
                let b = add(graph, model, 1, None);
                connect(graph, model, a, b);
            }
        }
        // Every reachable branch above creates at least one node, which unconditionally calls
        // `on_modification` in production (the early-return branch of `StableIndexHole`, which
        // makes no production call at all, returns before reaching this line).
        model.is_reduced = false;
    }

    /// Recompute every audited fact from `model`'s own state and compare it to `graph`.
    fn audit_dag_matches_model(
        graph: &FetchDependencyGraph,
        model: &DagModel,
    ) -> Result<(), TestCaseError> {
        // 1. Live node sets correspond exactly.
        let mut production_nodes: Vec<usize> =
            graph.graph.node_indices().map(|n| n.index()).collect();
        production_nodes.sort_unstable();
        let mut model_nodes: Vec<usize> = model.live.iter().map(|n| n.index()).collect();
        model_nodes.sort_unstable();
        prop_assert_eq!(production_nodes, model_nodes, "live node sets diverged");

        // 2. The production graph must remain acyclic.
        prop_assert!(
            petgraph::algo::toposort(&graph.graph, None).is_ok(),
            "production graph is not acyclic"
        );

        // 3. Direct edge endpoints match exactly.
        let mut production_edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        production_edges.sort_unstable();
        let mut model_edges: Vec<(usize, usize)> = model
            .edges
            .iter()
            .map(|&(s, t)| (s.index(), t.index()))
            .collect();
        model_edges.sort_unstable();
        prop_assert_eq!(production_edges, model_edges, "edge sets diverged");

        // 4. `is_reduced`/`is_optimized` match the model's invalidation state. This harness never
        // calls `reduce_and_optimize`, so `is_optimized` should never become true.
        prop_assert_eq!(graph.is_reduced, model.is_reduced, "is_reduced diverged");
        prop_assert!(!graph.is_optimized, "is_optimized unexpectedly set");

        // 5. If reduced, every retained edge must be indispensable.
        if model.is_reduced {
            let redundant = model.globally_redundant_edges();
            prop_assert!(
                redundant.is_empty(),
                "reduced graph retains a redundant edge: {redundant:?}"
            );
        }

        // 6. `reduced_defer_edges` ledger, compared as a multiset.
        let mut production_ledger: Vec<(usize, usize)> = graph
            .reduced_defer_edges
            .iter()
            .map(|&(s, t)| (s.index(), t.index()))
            .collect();
        production_ledger.sort_unstable();
        let mut model_ledger: Vec<(usize, usize)> = model
            .ledger
            .iter()
            .map(|&(s, t)| (s.index(), t.index()))
            .collect();
        model_ledger.sort_unstable();
        prop_assert_eq!(
            production_ledger,
            model_ledger,
            "reduced_defer_edges ledger diverged"
        );

        // 7. Every ledger entry refers to a live node.
        for &(s, t) in &graph.reduced_defer_edges {
            prop_assert!(model.live.contains(&s), "ledger source {s:?} is not live");
            prop_assert!(model.live.contains(&t), "ledger target {t:?} is not live");
        }

        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The core sentinel: after every command in a generated trace, the production graph's
        /// topology, `is_reduced`/`is_optimized` flags, and `reduced_defer_edges` ledger must
        /// match a naive reference model recomputed from scratch. See "Sentinel 1" in
        /// proptest-graph-notes.md.
        #[test]
        fn fetch_dependency_graph_trace_matches_naive_model(commands in dag_command_trace_strategy()) {
            let mut graph = make_test_dep_graph();
            let mut model = DagModel::default();
            for command in &commands {
                apply_dag_command(&mut graph, &mut model, command);
                audit_dag_matches_model(&graph, &model)?;
            }
        }

        /// Focused dense-index property: `to_toposorted_adjacency_list`'s rank mapping must round
        /// trip every edge back to its original `NodeIndex` pair, including across stable-index
        /// holes left by node removal.
        #[test]
        fn toposorted_adjacency_round_trips_edges_with_stable_index_holes(
            commands in dag_command_trace_strategy()
        ) {
            let mut graph = make_test_dep_graph();
            let mut model = DagModel::default();
            for command in &commands {
                apply_dag_command(&mut graph, &mut model, command);
            }

            let Some((adj, tred_index_of)) = to_toposorted_adjacency_list(&graph.graph) else {
                // This harness only ever builds a DAG (see the rank-ordering invariant above).
                return Err(TestCaseError::fail("expected an acyclic graph"));
            };

            let live_count = tred_index_of.iter().filter(|&&rank| rank != usize::MAX).count();
            let mut rank_to_node: Vec<Option<NodeIndex>> = vec![None; live_count];
            for (original_index, &rank) in tred_index_of.iter().enumerate() {
                if rank != usize::MAX {
                    rank_to_node[rank] = Some(NodeIndex::new(original_index));
                }
            }

            let mut round_tripped: Vec<(usize, usize)> = adj
                .edge_references()
                .map(|e| {
                    let source = rank_to_node[e.source().index()].expect("rank must map back to a node");
                    let target = rank_to_node[e.target().index()].expect("rank must map back to a node");
                    (source.index(), target.index())
                })
                .collect();
            round_tripped.sort_unstable();

            let mut actual: Vec<(usize, usize)> = graph
                .graph
                .edge_references()
                .map(|e| (e.source().index(), e.target().index()))
                .collect();
            actual.sort_unstable();

            prop_assert_eq!(round_tripped, actual);
        }
    }

    /// A -> B -> C: transitive reduction of a plain chain must remove nothing.
    #[test]
    fn dag_chain_has_no_redundant_edges() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", None);
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);

        graph.reduce();

        let mut edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        edges.sort_unstable();
        assert_eq!(edges, vec![(a.index(), b.index()), (b.index(), c.index())]);
        assert!(graph.reduced_defer_edges.is_empty());
    }

    /// A -> B -> C plus a direct A -> C shortcut: reduce must remove exactly the shortcut.
    #[test]
    fn dag_reduce_removes_shortcut_edge() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", None);
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);
        add_test_edge(&mut graph, a, c);

        graph.reduce();

        let mut edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        edges.sort_unstable();
        assert_eq!(edges, vec![(a.index(), b.index()), (b.index(), c.index())]);
        assert!(graph.is_reduced);
    }

    /// Diamond (A -> B, A -> C, B -> D, C -> D) plus a redundant direct A -> D shortcut: reduce
    /// must remove only the shortcut, keeping every diamond edge (none of them are individually
    /// redundant).
    #[test]
    fn dag_reduce_keeps_diamond_removes_shortcut() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", None);
        let d = add_test_node(&mut graph, "Subgraph1", None);
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, a, c);
        add_test_edge(&mut graph, b, d);
        add_test_edge(&mut graph, c, d);
        add_test_edge(&mut graph, a, d);

        graph.reduce();

        let mut edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        edges.sort_unstable();
        let mut expected = vec![
            (a.index(), b.index()),
            (a.index(), c.index()),
            (b.index(), d.index()),
            (c.index(), d.index()),
        ];
        expected.sort_unstable();
        assert_eq!(edges, expected);
    }

    /// Bow tie (A -> M, B -> M, M -> X, M -> Y) plus a redundant direct A -> X shortcut: reduce
    /// must remove only the shortcut.
    #[test]
    fn dag_reduce_keeps_bow_tie_removes_shortcut() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph1", None);
        let m = add_test_node(&mut graph, "Subgraph2", None);
        let x = add_test_node(&mut graph, "Subgraph1", None);
        let y = add_test_node(&mut graph, "Subgraph1", None);
        add_test_edge(&mut graph, a, m);
        add_test_edge(&mut graph, b, m);
        add_test_edge(&mut graph, m, x);
        add_test_edge(&mut graph, m, y);
        add_test_edge(&mut graph, a, x);

        graph.reduce();

        let mut edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        edges.sort_unstable();
        let mut expected = vec![
            (a.index(), m.index()),
            (b.index(), m.index()),
            (m.index(), x.index()),
            (m.index(), y.index()),
        ];
        expected.sort_unstable();
        assert_eq!(edges, expected);
    }

    /// Removing an interior node and adding a new one exercises a stable-index hole: the new
    /// node reuses the freed `NodeIndex`, and reduction must still behave correctly across it.
    #[test]
    fn dag_reduce_across_stable_index_hole() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", None);
        graph.remove_node(b);
        // Reuses `b`'s freed slot; `d.index() == b.index()` is the point of this test.
        let d = add_test_node(&mut graph, "Subgraph1", None);
        assert_eq!(d.index(), b.index());

        add_test_edge(&mut graph, a, d);
        add_test_edge(&mut graph, d, c);
        add_test_edge(&mut graph, a, c);

        graph.reduce();

        let mut edges: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        edges.sort_unstable();
        assert_eq!(edges, vec![(a.index(), d.index()), (d.index(), c.index())]);
    }

    /// A(primary) -> B(primary) -> C(deferred), plus a direct A -> C shortcut crossing the defer
    /// boundary: `reduce` (not just `remove_redundant_edges`) must ledger the shortcut so
    /// `collect_reduced_defer_dependencies` can still recover the dependency later.
    #[test]
    fn dag_reduce_records_defer_crossing_shortcut_in_ledger() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);
        add_test_edge(&mut graph, a, c);

        graph.reduce();

        assert_eq!(graph.reduced_defer_edges, vec![(a, c)]);
    }

    /// Calling `reduce` a second time on an already-reduced graph must be a complete no-op:
    /// edges, ledger, and flags are all unchanged.
    #[test]
    fn dag_reduce_is_idempotent() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", Some("defer1"));
        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);
        add_test_edge(&mut graph, a, c);

        graph.reduce();
        let edges_after_first: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        let ledger_after_first = graph.reduced_defer_edges.clone();
        assert!(graph.is_reduced);

        graph.reduce();
        let edges_after_second: Vec<(usize, usize)> = graph
            .graph
            .edge_references()
            .map(|e| (e.source().index(), e.target().index()))
            .collect();
        assert_eq!(edges_after_first, edges_after_second);
        assert_eq!(ledger_after_first, graph.reduced_defer_edges);
        assert!(graph.is_reduced);
    }

    // =========================================================================================
    // ProcessingState sub-property (proptest-graph-notes.md, Sentinel 1 / "Graph PR 2").
    //
    // `ProcessingState` is a separate exact state machine over the same generated DAG. Two kinds
    // of property here:
    //   - `create_state_for_children_of_processed_node` is compared against a naive
    //     recomputation over the Sentinel-1 `DagModel` graph (an oracle test).
    //   - `merge_with`/`update_for_processed_nodes` are pure data operations on `ProcessingState`
    //     itself (no `&FetchDependencyGraph` involved), so their laws are checked directly against
    //     synthetically generated, internally well-formed states, with no graph needed at all.
    // =========================================================================================

    /// Snapshot a `ProcessingState` into a normalized, order-independent form for comparison:
    /// sorted `next` indices, and sorted `(node index, sorted parent indices)` pairs for
    /// `unhandled`. `path_in_parent` is always `None` in every state this harness builds, so the
    /// index alone identifies a `ParentRelation`.
    fn processing_state_snapshot(
        state: &ProcessingState,
    ) -> (Vec<usize>, Vec<(usize, Vec<usize>)>) {
        let mut next: Vec<usize> = state.next.iter().map(|n| n.index()).collect();
        next.sort_unstable();
        let mut unhandled: Vec<(usize, Vec<usize>)> = state
            .unhandled
            .iter()
            .map(|u| {
                let mut parents: Vec<usize> = u
                    .unhandled_parents
                    .iter()
                    .map(|p| p.parent_node_id.index())
                    .collect();
                parents.sort_unstable();
                (u.node.index(), parents)
            })
            .collect();
        unhandled.sort_unstable();
        (next, unhandled)
    }

    /// `ProcessingState`/`UnhandledNode` don't derive `Clone` (their production-visible API is
    /// consuming-by-value), so tests that need the same state twice rebuild it field-by-field.
    fn clone_processing_state(state: &ProcessingState) -> ProcessingState {
        ProcessingState {
            next: state.next.clone(),
            unhandled: state
                .unhandled
                .iter()
                .map(|u| UnhandledNode {
                    node: u.node,
                    unhandled_parents: u.unhandled_parents.clone(),
                })
                .collect(),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// `create_state_for_children_of_processed_node` must exactly match a naive
        /// recomputation from the Sentinel-1 `DagModel`: a child is "next" iff `processed_index`
        /// is its only parent, otherwise "unhandled" with every other parent still outstanding.
        #[test]
        fn create_state_for_children_of_processed_node_matches_naive_recomputation(
            commands in dag_command_trace_strategy(),
            pick in any::<u16>(),
        ) {
            let mut graph = make_test_dep_graph();
            let mut model = DagModel::default();
            for command in &commands {
                apply_dag_command(&mut graph, &mut model, command);
            }
            let Some(processed_index) = model.pick(pick) else {
                return Ok(());
            };
            let children: Vec<NodeIndex> = model.children(processed_index).collect();

            let mut expected_next = Vec::new();
            let mut expected_unhandled: Vec<(usize, Vec<usize>)> = Vec::new();
            for &child in &children {
                let parents: Vec<NodeIndex> = model.parents(child).collect();
                if parents.len() == 1 {
                    expected_next.push(child.index());
                } else {
                    let mut remaining: Vec<usize> = parents
                        .iter()
                        .filter(|&&p| p != processed_index)
                        .map(|p| p.index())
                        .collect();
                    remaining.sort_unstable();
                    expected_unhandled.push((child.index(), remaining));
                }
            }
            expected_next.sort_unstable();
            expected_unhandled.sort_unstable();

            let actual = graph.create_state_for_children_of_processed_node(processed_index, children);
            let (actual_next, actual_unhandled) = processing_state_snapshot(&actual);
            prop_assert_eq!(actual_next.clone(), expected_next);
            prop_assert_eq!(actual_unhandled.clone(), expected_unhandled);

            // Well-formedness is guaranteed here because a single real call can't produce the
            // cross-state conflicts that make it inapplicable to the pure-algebra properties
            // below (see their doc comments).
            let next_set: IndexSet<usize> = actual_next.iter().copied().collect();
            prop_assert_eq!(next_set.len(), actual_next.len(), "duplicate `next` entry");
            let unhandled_nodes: Vec<usize> = actual_unhandled.iter().map(|(n, _)| *n).collect();
            let unhandled_node_set: IndexSet<usize> = unhandled_nodes.iter().copied().collect();
            prop_assert_eq!(
                unhandled_node_set.len(),
                unhandled_nodes.len(),
                "duplicate `unhandled` entry"
            );
            for node in &unhandled_nodes {
                prop_assert!(!next_set.contains(node), "node {node} in both next and unhandled");
            }
            for (_, parents) in &actual_unhandled {
                prop_assert!(!parents.is_empty(), "unhandled entry with no remaining parents");
            }
        }

        /// `merge_with` is commutative, associative, and idempotent as a *set* operation (vector
        /// order is incidental).
        ///
        /// The three states being merged are NOT independently generated: an earlier version of
        /// this property did that and found a real associativity failure, which turned out to be
        /// a generator bug rather than a production one (see the long comment above
        /// `dag_consistent_sibling_states_strategy` for the full analysis and the discarded
        /// counterexample). `merge_with`'s only caller (`process_nodes`) always merges states
        /// that are per-sibling *views of the same underlying parent sets* — each state's opinion
        /// about a shared descendant differs only in which one already-processed sibling has been
        /// subtracted from that descendant's true parent set. Three views that disagree about a
        /// descendant's true parent set (e.g. one claims it's `{0, A}`, another claims `{1, B}`)
        /// can't arise from real siblings and aren't a precondition `merge_with` is expected to
        /// tolerate, so the generator below constructs three such internally-consistent views
        /// from one shared ground truth instead of three unrelated random states.
        #[test]
        fn processing_state_merge_with_is_commutative_associative_idempotent(
            full_parents in dag_shared_full_parent_sets_strategy(),
        ) {
            let state_a = || dag_sibling_view(&full_parents, PS_SIBLINGS[0]);
            let state_b = || dag_sibling_view(&full_parents, PS_SIBLINGS[1]);
            let state_c = || dag_sibling_view(&full_parents, PS_SIBLINGS[2]);

            let commutative_ab = state_a().merge_with(state_b());
            let commutative_ba = state_b().merge_with(state_a());
            prop_assert_eq!(
                processing_state_snapshot(&commutative_ab),
                processing_state_snapshot(&commutative_ba),
                "merge_with is not commutative"
            );

            let assoc_left = state_a().merge_with(state_b()).merge_with(state_c());
            let assoc_right = state_a().merge_with(state_b().merge_with(state_c()));
            prop_assert_eq!(
                processing_state_snapshot(&assoc_left),
                processing_state_snapshot(&assoc_right),
                "merge_with is not associative"
            );

            let idempotent = state_a().merge_with(state_a());
            prop_assert_eq!(
                processing_state_snapshot(&idempotent),
                processing_state_snapshot(&state_a()),
                "merge_with(a, a) must equal a"
            );
        }

        /// `update_for_processed_nodes` is a pure filter over each `unhandled` entry's remaining
        /// parents. For disjoint `p1`/`p2`, applying it twice (once per set) must equal applying
        /// it once to their union; applying it to the empty set is the identity; applying the
        /// same set twice is idempotent.
        #[test]
        fn processing_state_update_for_processed_nodes_matches_sequential_application(
            state in dag_processing_state_strategy(),
            p1 in prop::collection::vec(0..PS_PARENT_UNIVERSE, 0..4),
            p2_raw in prop::collection::vec(0..PS_PARENT_UNIVERSE, 0..4),
        ) {
            let p1: Vec<NodeIndex> = {
                let mut v: Vec<usize> = p1;
                v.sort_unstable();
                v.dedup();
                v.into_iter().map(NodeIndex::new).collect()
            };
            // Keep `p2` disjoint from `p1`, per the law's precondition.
            let p2: Vec<NodeIndex> = {
                let mut v: Vec<usize> = p2_raw;
                v.sort_unstable();
                v.dedup();
                v.into_iter()
                    .filter(|n| !p1.contains(&NodeIndex::new(*n)))
                    .map(NodeIndex::new)
                    .collect()
            };

            let identity = clone_processing_state(&state).update_for_processed_nodes(&[]);
            prop_assert_eq!(
                processing_state_snapshot(&identity),
                processing_state_snapshot(&state),
                "update_for_processed_nodes(&[]) must be the identity"
            );

            let mut union = p1.clone();
            union.extend(p2.iter().copied());
            let union_applied = clone_processing_state(&state).update_for_processed_nodes(&union);
            let sequential_applied = clone_processing_state(&state)
                .update_for_processed_nodes(&p1)
                .update_for_processed_nodes(&p2);
            prop_assert_eq!(
                processing_state_snapshot(&union_applied),
                processing_state_snapshot(&sequential_applied),
                "update_for_processed_nodes(p1 ++ p2) must equal update(p1).update(p2) for disjoint p1/p2"
            );

            let applied_once = clone_processing_state(&state).update_for_processed_nodes(&p1);
            let applied_twice = clone_processing_state(&state)
                .update_for_processed_nodes(&p1)
                .update_for_processed_nodes(&p1);
            prop_assert_eq!(
                processing_state_snapshot(&applied_once),
                processing_state_snapshot(&applied_twice),
                "update_for_processed_nodes(p1) must be idempotent"
            );
        }
    }

    /// Universe sizes for `dag_processing_state_strategy`: small enough to shrink well and to
    /// make node/parent collisions (needed to exercise `merge_with`'s intersection logic) common.
    const PS_NODE_UNIVERSE: usize = 6;
    const PS_PARENT_UNIVERSE: usize = 6;

    /// Generate an internally well-formed `ProcessingState`: no duplicate `next` entry, no
    /// duplicate `unhandled` node, no node in both, and every `unhandled` entry has at least one
    /// (deduplicated) parent — the invariant `ProcessingState`'s own doc comment states
    /// ("we make sure that this never hold node with no edges").
    fn dag_processing_state_strategy() -> impl Strategy<Value = ProcessingState> {
        prop::collection::vec(0..PS_NODE_UNIVERSE, 0..PS_NODE_UNIVERSE).prop_flat_map(|raw| {
            let mut tracked = raw;
            tracked.sort_unstable();
            tracked.dedup();
            let n = tracked.len();
            (
                prop::collection::vec(any::<bool>(), n),
                prop::collection::vec(prop::collection::vec(0..PS_PARENT_UNIVERSE, 1..4), n),
            )
                .prop_map(move |(is_next_flags, parent_choices)| {
                    let mut next = Vec::new();
                    let mut unhandled = Vec::new();
                    for (i, &node_idx) in tracked.iter().enumerate() {
                        let node = NodeIndex::new(node_idx);
                        if is_next_flags[i] {
                            next.push(node);
                        } else {
                            let mut parents = parent_choices[i].clone();
                            parents.sort_unstable();
                            parents.dedup();
                            let unhandled_parents = parents
                                .into_iter()
                                .map(|p| ParentRelation {
                                    parent_node_id: NodeIndex::new(p),
                                    path_in_parent: None,
                                })
                                .collect();
                            unhandled.push(UnhandledNode {
                                node,
                                unhandled_parents,
                            });
                        }
                    }
                    ProcessingState { next, unhandled }
                })
        })
    }

    /// Three fixed "already-processed sibling" identifiers, deliberately far outside the
    /// candidate/background-parent ranges below so they can never collide with an ordinary
    /// parent by coincidence.
    const PS_SIBLINGS: [usize; 3] = [1_000, 1_001, 1_002];
    const PS_CANDIDATE_UNIVERSE: usize = 5;
    const PS_BACKGROUND_PARENT_UNIVERSE: usize = 4;
    /// Offset applied to background-parent ids so they can never collide with `PS_SIBLINGS`.
    const PS_BACKGROUND_OFFSET: usize = 5_000;

    /// Ground truth for `dag_sibling_view`: for each of a small number of candidate nodes, its
    /// true, complete parent set — a subset of `PS_SIBLINGS` plus zero or more "background"
    /// parents that no generated sibling view ever accounts for (so some candidates stay
    /// permanently unhandled no matter which siblings are merged, exactly like a real fetch node
    /// that also depends on a fetch outside the batch being processed).
    fn dag_shared_full_parent_sets_strategy() -> impl Strategy<Value = Vec<Vec<usize>>> {
        prop::collection::vec(
            (
                prop::collection::vec(0..PS_SIBLINGS.len(), 0..PS_SIBLINGS.len()),
                prop::collection::vec(0..PS_BACKGROUND_PARENT_UNIVERSE, 0..3),
            ),
            PS_CANDIDATE_UNIVERSE,
        )
        .prop_map(|per_candidate| {
            per_candidate
                .into_iter()
                .map(|(sibling_idxs, background)| {
                    let mut parents: Vec<usize> =
                        sibling_idxs.into_iter().map(|i| PS_SIBLINGS[i]).collect();
                    parents.extend(background.into_iter().map(|b| PS_BACKGROUND_OFFSET + b));
                    parents.sort_unstable();
                    parents.dedup();
                    parents
                })
                .collect()
        })
    }

    /// The `ProcessingState` a single sibling (`excluded_sibling`) would compute via
    /// `create_state_for_children_of_processed_node`, given the shared ground truth
    /// `full_parents`: a candidate is in this view at all only if `excluded_sibling` is really
    /// one of its parents (mirroring "only this sibling's own children are candidates"), and its
    /// remaining parents are every other true parent.
    fn dag_sibling_view(full_parents: &[Vec<usize>], excluded_sibling: usize) -> ProcessingState {
        let mut next = Vec::new();
        let mut unhandled = Vec::new();
        for (candidate_idx, parents) in full_parents.iter().enumerate() {
            if !parents.contains(&excluded_sibling) {
                continue;
            }
            let node = NodeIndex::new(candidate_idx);
            let remaining: Vec<ParentRelation> = parents
                .iter()
                .copied()
                .filter(|&p| p != excluded_sibling)
                .map(|p| ParentRelation {
                    parent_node_id: NodeIndex::new(p),
                    path_in_parent: None,
                })
                .collect();
            if remaining.is_empty() {
                next.push(node);
            } else {
                unhandled.push(UnhandledNode {
                    node,
                    unhandled_parents: remaining,
                });
            }
        }
        ProcessingState { next, unhandled }
    }

    // =========================================================================================
    // Defer dependency extension (proptest-graph-notes.md, Sentinel 1 / "Graph PR 2"): real
    // top-level field tokens, `merge_at = None`, no aliases, no fragments — the first payload
    // stage the notes describe.
    //
    // The rule under test, from `collect_reduced_defer_dependencies`'s own doc comment: a removed
    // cross-defer edge source -> target must be recovered as a defer dependency iff the source's
    // selection shares a field (other than `__typename`) with one of the target's `_entities`
    // input selections. This builds the canonical three-node shape from the notes
    // (`A(primary) -> B(primary) -> C(deferred)`, plus a redundant `A -> C` shortcut, reduced via
    // the real `reduce()` — not hand-pushed into the ledger) with real field selections on `A`
    // and real `_entities` inputs on `C`, and compares the recovered dependency against a direct
    // field-name-set-intersection oracle.
    // =========================================================================================

    /// Which of the shared test fixture's three real `T` fields (`id`, `v1`, `v2` — see
    /// `TEST_SUPERGRAPH_SDL`), plus `__typename`, a selection includes. This is the fixed field
    /// universe the notes recommend materializing tokens from. `__typename` is kept separate
    /// because the production rule explicitly excludes it from intersection.
    #[derive(Debug, Clone, Copy)]
    struct DagFieldSelection {
        id: bool,
        v1: bool,
        v2: bool,
        typename: bool,
    }

    fn dag_field_selection_strategy() -> impl Strategy<Value = DagFieldSelection> {
        any::<(bool, bool, bool, bool)>().prop_map(|(id, v1, v2, typename)| {
            // A field set string can't be empty; if every flag came back false, force one real
            // field on rather than falling back to `__typename` alone, so the "no real overlap"
            // case is still exercised as often as the "some overlap" case.
            let id = id || !(v1 || v2 || typename);
            DagFieldSelection {
                id,
                v1,
                v2,
                typename,
            }
        })
    }

    fn dag_field_selection_string(selection: DagFieldSelection) -> String {
        let mut parts = Vec::new();
        if selection.typename {
            parts.push("__typename");
        }
        if selection.id {
            parts.push("id");
        }
        if selection.v1 {
            parts.push("v1");
        }
        if selection.v2 {
            parts.push("v2");
        }
        parts.join(" ")
    }

    /// The oracle: field-name-set intersection, `__typename` excluded — independent of
    /// `has_field_intersection`'s `SelectionMap`/inline-fragment-aware traversal.
    fn dag_field_selections_intersect(a: DagFieldSelection, b: DagFieldSelection) -> bool {
        (a.id && b.id) || (a.v1 && b.v1) || (a.v2 && b.v2)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// See the module doc comment above for the full rule and shape being tested.
        #[test]
        fn collect_reduced_defer_dependencies_matches_field_intersection(
            source_selection in dag_field_selection_strategy(),
            target_selection in dag_field_selection_strategy(),
            defer_choice in 0u8..4,
        ) {
            let mut graph = make_test_dep_graph();
            // `a` and `c` are both in Subgraph2: per the fixture SDL, `v1`/`v2` are owned only by
            // Subgraph2 (`id` is the shared key field, present in both), so Subgraph2 is the only
            // subgraph schema against which all three real `T` fields can be parsed.
            let a = add_test_node(&mut graph, "Subgraph2", None);
            let b = add_test_node(&mut graph, "Subgraph1", None);
            let defer_label = format!("d{defer_choice}");
            let c = add_test_node(&mut graph, "Subgraph2", Some(defer_label.as_str()));

            // Give `a` a real top-level selection (the source's tokens).
            let a_subgraph_schema = graph.federated_query_graph.schema_by_source("Subgraph2").unwrap().clone();
            let source_fields = parse_field_set(
                &a_subgraph_schema,
                name!("T"),
                &dag_field_selection_string(source_selection),
                true,
            )
            .unwrap();
            Arc::make_mut(graph.graph.node_weight_mut(a).unwrap())
                .selection_set_mut()
                .add_selections(&Arc::new(source_fields))
                .unwrap();

            // Give `c` real `_entities` inputs (the target's tokens). These are built against the
            // supergraph schema, matching `FetchInputs::add`'s precondition; `SelectionKey` is
            // schema-independent (response name + directives), so intersection with `a`'s
            // subgraph-schema-rooted selection still matches by field name.
            let supergraph_schema = graph.supergraph_schema.clone();
            let target_fields = parse_field_set(
                &supergraph_schema,
                name!("T"),
                &dag_field_selection_string(target_selection),
                true,
            )
            .unwrap();
            Arc::make_mut(graph.graph.node_weight_mut(c).unwrap())
                .add_inputs(&target_fields, iter::empty())
                .unwrap();

            add_test_edge(&mut graph, a, b);
            add_test_edge(&mut graph, b, c);
            add_test_edge(&mut graph, a, c);

            graph.reduce();
            prop_assert_eq!(
                graph.reduced_defer_edges.clone(),
                vec![(a, c)],
                "the shortcut must be ledgered before the dependency-recovery rule can be exercised"
            );

            let mut deps = Vec::new();
            graph.collect_reduced_defer_dependencies(a, &mut deps).unwrap();

            if dag_field_selections_intersect(source_selection, target_selection) {
                let id = *graph
                    .graph
                    .node_weight(a)
                    .unwrap()
                    .id
                    .get()
                    .expect("a's fetch id must be assigned once a dependency is recorded");
                prop_assert_eq!(deps, vec![(defer_label, format!("{id}"))]);
            } else {
                prop_assert!(
                    deps.is_empty(),
                    "recovered a dependency despite no shared field: {deps:?}"
                );
            }
        }
    }

    /// `__typename` alone must never be enough to recover a dependency, even though it's a valid,
    /// non-empty selection on both sides (the property above already covers this via generated
    /// cases, but the exact all-`__typename` combination is worth a deterministic witness).
    #[test]
    fn dag_collect_reduced_defer_dependencies_ignores_typename_only_overlap() {
        let mut graph = make_test_dep_graph();
        let a = add_test_node(&mut graph, "Subgraph1", None);
        let b = add_test_node(&mut graph, "Subgraph2", None);
        let c = add_test_node(&mut graph, "Subgraph2", Some("defer1"));

        let subgraph1_schema = graph
            .federated_query_graph
            .schema_by_source("Subgraph1")
            .unwrap()
            .clone();
        let source_fields =
            parse_field_set(&subgraph1_schema, name!("T"), "__typename", true).unwrap();
        Arc::make_mut(graph.graph.node_weight_mut(a).unwrap())
            .selection_set_mut()
            .add_selections(&Arc::new(source_fields))
            .unwrap();

        let supergraph_schema = graph.supergraph_schema.clone();
        let target_fields =
            parse_field_set(&supergraph_schema, name!("T"), "__typename", true).unwrap();
        Arc::make_mut(graph.graph.node_weight_mut(c).unwrap())
            .add_inputs(&target_fields, iter::empty())
            .unwrap();

        add_test_edge(&mut graph, a, b);
        add_test_edge(&mut graph, b, c);
        add_test_edge(&mut graph, a, c);
        graph.reduce();
        assert_eq!(graph.reduced_defer_edges, vec![(a, c)]);

        let mut deps = Vec::new();
        graph
            .collect_reduced_defer_dependencies(a, &mut deps)
            .unwrap();
        assert!(deps.is_empty(), "expected no dependency, got {deps:?}");
    }
}
