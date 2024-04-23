use crate::error::{FederationError, SingleFederationError};
use crate::query_graph::graph_path::{
    selection_of_element, OpGraphPathContext, OpGraphPathTrigger, OpPath, OpPathElement,
};
use crate::query_graph::path_tree::{OpPathTree, PathTreeChild};
use crate::query_graph::{QueryGraph, QueryGraphEdgeTransition};
use crate::query_plan::conditions::{remove_conditions_from_selection_set, Conditions};
use crate::query_plan::fetch_dependency_graph_processor::FetchDependencyGraphProcessor;
use crate::query_plan::operation::normalized_field_selection::{
    NormalizedField, NormalizedFieldData,
};
use crate::query_plan::operation::{
    NormalizedOperation, NormalizedSelection, NormalizedSelectionSet, RebasedFragments,
    TYPENAME_FIELD,
};
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::{FetchDataRewrite, QueryPlanCost};
use crate::schema::position::{
    CompositeTypeDefinitionPosition, FieldDefinitionPosition, ObjectTypeDefinitionPosition,
    SchemaRootDefinitionKind,
};
use crate::schema::ValidFederationSchema;
use crate::subgraph::spec::{ANY_SCALAR_NAME, ENTITIES_QUERY};
use apollo_compiler::ast::{OperationType, Type};
use apollo_compiler::executable::{self, VariableDefinition};
use apollo_compiler::schema::{self, Name};
use apollo_compiler::{Node, NodeStr};
use indexmap::{IndexMap, IndexSet};
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use std::collections::HashSet;
use std::sync::Arc;

use super::operation::normalized_selection_map::NormalizedSelectionMap;
use crate::query_graph::extract_subgraphs_from_supergraph::FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME;
use crate::query_graph::extract_subgraphs_from_supergraph::FEDERATION_REPRESENTATIONS_VAR_NAME;

/// Represents a subgraph fetch of a query plan.
// PORT_NOTE: The JS codebase called this `FetchGroup`, but this naming didn't make it apparent that
// this was a node in a fetch dependency graph, so we've renamed it accordingly.
//
// The JS codebase additionally has a property named `subgraphAndMergeAtKey` that was used as a
// precomputed map key, but this isn't necessary in Rust since we can use `PartialEq`/`Eq`/`Hash`.
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphNode {
    /// The subgraph this fetch is queried against.
    pub(crate) subgraph_name: NodeStr,
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
    pub(crate) selection_set: Arc<NormalizedSelectionSet>,
    /// The conditions determining whether the fetch should be executed (which must be recomputed
    /// from the selection set when it changes).
    pub(crate) conditions: Conditions,
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

type FetchDependencyGraphPetgraph =
    StableDiGraph<Arc<FetchDependencyGraphNode>, Arc<FetchDependencyGraphEdge>>;

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
    graph: FetchDependencyGraphPetgraph,
    /// The root nodes by subgraph name, representing the fetches against root operation types of
    /// the subgraphs.
    root_nodes_by_subgraph: IndexMap<NodeStr, NodeIndex>,
    /// Tracks metadata about deferred blocks and their dependencies on one another.
    pub(crate) defer_tracking: DeferTracking,
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
    pub(crate) top_level_deferred: IndexSet<NodeStr>,
    pub(crate) deferred: IndexMap<NodeStr, DeferredInfo>,
    pub(crate) primary_selection: Option<Arc<NormalizedSelectionSet>>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct DeferredInfo {
    pub(crate) label: NodeStr,
    pub(crate) path: FetchDependencyGraphPath,
    pub(crate) sub_selection: NormalizedSelectionSet,
    pub(crate) deferred: IndexSet<NodeStr>,
    pub(crate) dependencies: IndexSet<NodeStr>,
}

// TODO: Write docstrings
#[derive(Debug, Clone)]
pub(crate) struct FetchDependencyGraphPath {
    pub(crate) full_path: OpPath,
    pub(crate) path_in_node: OpPath,
    pub(crate) response_path: Vec<FetchDataPathElement>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FetchDependencyGraphNodePath {
    full_path: Arc<OpPath>,
    path_in_node: Arc<OpPath>,
    response_path: Vec<FetchDataPathElement>,
}

#[derive(Debug, Clone)]
pub(crate) struct DeferContext {
    current_defer_ref: Option<NodeStr>,
    path_to_defer_parent: Arc<OpPath>,
    active_defer_ref: Option<NodeStr>,
    is_part_of_query: bool,
}

struct ParentRelation<'a> {
    parent_node_id: NodeIndex,
    path_in_parent: Option<&'a Arc<OpPath>>,
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

impl FetchDependencyGraphNodePath {
    fn for_new_key_fetch(&self, new_context: Arc<OpPath>) -> Self {
        Self {
            full_path: self.full_path.clone(),
            path_in_node: new_context,
            response_path: self.response_path.clone(),
        }
    }

    fn add(
        &self,
        element: Arc<OpPathElement>,
    ) -> Result<FetchDependencyGraphNodePath, FederationError> {
        Ok(Self {
            response_path: self.updated_response_path(&element)?,
            full_path: Arc::new(self.full_path.with_pushed(element.clone())),
            path_in_node: Arc::new(self.path_in_node.with_pushed(element)),
        })
    }

    fn updated_response_path(
        &self,
        element: &OpPathElement,
    ) -> Result<Vec<FetchDataPathElement>, FederationError> {
        let mut new_path = self.response_path.clone();
        if let OpPathElement::Field(field) = element {
            new_path.push(FetchDataPathElement::Key(
                field.data().response_name().into(),
            ));
            // TODO: is there a simpler we to find a field’s type from `&NormalizedField`?
            let mut type_ = &field
                .data()
                .field_position
                .get(field.data().schema.schema())?
                .ty;
            loop {
                match type_ {
                    schema::Type::Named(_) | schema::Type::NonNullNamed(_) => break,
                    schema::Type::List(inner) | schema::Type::NonNullList(inner) => {
                        new_path.push(FetchDataPathElement::AnyIndex);
                        type_ = inner
                    }
                }
            }
        };
        Ok(new_path)
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
            None,
            None,
        )?;
        self.root_nodes_by_subgraph
            .insert(subgraph_name.clone(), node);
        Ok(node)
    }

    fn new_root_type_node(
        &mut self,
        subgraph_name: NodeStr,
        root_kind: SchemaRootDefinitionKind,
        parent_type: &ObjectTypeDefinitionPosition,
        merge_at: Option<Vec<FetchDataPathElement>>,
        defer_ref: Option<NodeStr>,
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
        subgraph_name: NodeStr,
        parent_type: CompositeTypeDefinitionPosition,
        has_inputs: bool,
        root_kind: SchemaRootDefinitionKind,
        merge_at: Option<Vec<FetchDataPathElement>>,
        defer_ref: Option<NodeStr>,
    ) -> Result<NodeIndex, FederationError> {
        let subgraph_schema = self
            .federated_query_graph
            .schema_by_source(&subgraph_name)?
            .clone();
        self.on_modification();
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
            id: None,
            defer_ref,
            cached_cost: None,
            must_preserve_selection_set: false,
            is_known_useful: false,
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

    pub(crate) fn edge_weight(
        &self,
        edge: EdgeIndex,
    ) -> Result<&Arc<FetchDependencyGraphEdge>, FederationError> {
        self.graph
            .edge_weight(edge)
            .ok_or_else(|| FederationError::internal("Edge unexpectedly missing".to_owned()))
    }

    /// Does not take `&mut self` so that other fields can be mutated while this borrow lasts
    fn edge_weight_mut(
        graph: &mut FetchDependencyGraphPetgraph,
        edge: EdgeIndex,
    ) -> Result<&mut FetchDependencyGraphEdge, FederationError> {
        Ok(Arc::make_mut(graph.edge_weight_mut(edge).ok_or_else(
            || FederationError::internal("Edge unexpectedly missing"),
        )?))
    }

    fn get_or_create_key_node(
        &mut self,
        subgraph_name: &NodeStr,
        merge_at: &[FetchDataPathElement],
        type_: &CompositeTypeDefinitionPosition,
        parent: ParentRelation<'_>,
        conditions_nodes: &IndexSet<NodeIndex>,
        defer_ref: Option<&NodeStr>,
    ) -> Result<NodeIndex, FederationError> {
        // Let's look if we can reuse a node we have, that is an existing child of the parent that:
        // 1. is for the same subgraph
        // 2. has the same merge_at
        // 3. is for the same entity type (we don't reuse nodes for different entities just yet,
        //    as this can create unecessary dependencies that gets in the way of some optimizations;
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
        for existing_id in self.children_of(parent.parent_node_id) {
            let existing = self.node_weight(existing_id)?;
            if existing.subgraph_name == *subgraph_name
                && existing.merge_at.as_deref() == Some(merge_at)
                && existing
                    .selection_set
                    .selection_set
                    .selections
                    .values()
                    .all(|selection| {
                        matches!(
                            selection,
                            NormalizedSelection::InlineFragment(fragment)
                            if fragment.casted_type() == type_
                        )
                    })
                && !self.is_in_nodes_or_their_ancestors(existing_id, conditions_nodes)
                && existing.defer_ref.as_ref() == defer_ref
                && self
                    .parents_relations_of(existing_id)
                    .find(|rel| rel.parent_node_id == parent.parent_node_id)
                    .and_then(|rel| rel.path_in_parent)
                    == parent.path_in_parent
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
        subgraph_name: &NodeStr,
        merge_at: Vec<FetchDataPathElement>,
        defer_ref: Option<NodeStr>,
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
    fn add_parent(&mut self, child_id: NodeIndex, parent_relation: ParentRelation<'_>) {
        let ParentRelation {
            parent_node_id,
            path_in_parent,
        } = parent_relation;
        if self.graph.contains_edge(parent_node_id, child_id) {
            return;
        }
        assert!(
            !self.graph.contains_edge(child_id, parent_node_id),
            "Group {parent_node_id:?} is a child of {child_id:?}: \
             adding it as parent would create a cycle"
        );
        self.on_modification();
        self.graph.add_edge(
            parent_node_id,
            child_id,
            Arc::new(FetchDependencyGraphEdge {
                path: path_in_parent.cloned(),
            }),
        );
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

        // No risk of inifite loop as the graph is acyclic:
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

    fn children_of(&self, node_id: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.graph
            .neighbors_directed(node_id, petgraph::Direction::Outgoing)
    }

    fn parents_of(&self, node_id: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.graph
            .neighbors_directed(node_id, petgraph::Direction::Incoming)
    }

    fn parents_relations_of(
        &self,
        node_id: NodeIndex,
    ) -> impl Iterator<Item = ParentRelation<'_>> + '_ {
        self.graph
            .edges_directed(node_id, petgraph::Direction::Incoming)
            .map(|edge| ParentRelation {
                parent_node_id: edge.source(),
                path_in_parent: edge.weight().path.as_ref(),
            })
    }

    fn type_for_fetch_inputs(
        &self,
        type_name: &Name,
    ) -> Result<CompositeTypeDefinitionPosition, FederationError> {
        self.supergraph_schema
            .get_type(type_name.clone())?
            .try_into()
    }

    /// Do a transitive reduction (https://en.wikipedia.org/wiki/Transitive_reduction) of the graph
    /// We keep it simple and do a DFS from each vertex. The complexity is not amazing, but dependency
    /// graphs between fetch groups will almost surely never be huge and query planning performance
    /// is not paramount so this is almost surely "good enough".
    fn reduce(&mut self) {
        if std::mem::replace(&mut self.is_reduced, true) {
            return;
        }

        for _node in self.graph.node_weights_mut() {
            // TODO Reduce: FED-16
        }
    }

    /// Reduce the graph (see `reduce`) and then do a some additional traversals to optimize for:
    ///  1) fetches with no selection: this can happen when we have a require if the only field requested
    ///     was the one with the require and that forced some dependencies. Those fetch should have
    ///     no dependents and we can just remove them.
    ///  2) fetches that are made in parallel to the same subgraph and the same path, and merge those.
    fn reduce_and_optimize(&mut self) {
        if std::mem::replace(&mut self.is_optimized, true) {
            return;
        }

        self.reduce();

        // TODO Optimize: FED-55
    }

    /// Processes the "plan" represented by this query graph using the provided `processor`.
    ///
    /// Returns a main part and a (potentially empty) deferred part.
    pub(crate) fn process<TProcessed, TDeferred>(
        &mut self,
        _processor: impl FetchDependencyGraphProcessor<TProcessed, TDeferred>,
        _root_kind: SchemaRootDefinitionKind,
    ) -> Result<(TProcessed, Vec<TDeferred>), FederationError> {
        self.reduce_and_optimize();

        todo!("FED-146")
    }
}

impl FetchDependencyGraphNode {
    pub(crate) fn selection_set_mut(&mut self) -> &mut FetchSelectionSet {
        self.cached_cost = None;
        &mut self.selection_set
    }

    fn add_inputs(
        &mut self,
        supergraph_schema: &ValidFederationSchema,
        selection: &NormalizedSelectionSet,
        rewrites: impl IntoIterator<Item = Arc<FetchDataRewrite>>,
    ) {
        let inputs = self.inputs.get_or_insert_with(|| {
            Arc::new(FetchInputs {
                selection_sets_per_parent_type: Default::default(),
                supergraph_schema: supergraph_schema.clone(),
            })
        });
        Arc::make_mut(inputs).add(selection);
        Arc::make_mut(&mut self.input_rewrites).extend(rewrites);
    }

    pub(crate) fn cost(&mut self) -> Result<QueryPlanCost, FederationError> {
        if self.cached_cost.is_none() {
            self.cached_cost = Some(self.selection_set.selection_set.cost(1)?)
        }
        Ok(self.cached_cost.unwrap())
    }

    pub(crate) fn to_plan_node(
        &self,
        query_graph: &QueryGraph,
        handled_conditions: &Conditions,
        variable_definitions: &[Node<VariableDefinition>],
        fragments: Option<&mut RebasedFragments>,
        operation_name: Option<NodeStr>,
    ) -> Result<Option<super::PlanNode>, FederationError> {
        if self.selection_set.selection_set.selections.is_empty() {
            return Ok(None);
        }
        let (selection, output_rewrites) =
            self.finalize_selection(variable_definitions, handled_conditions, &fragments)?;
        let input_nodes = self
            .inputs
            .as_ref()
            .map(|inputs| {
                inputs.to_selection_set_nodes(
                    variable_definitions,
                    handled_conditions,
                    &self.parent_type,
                )
            })
            .transpose()?;
        let subgraph_schema = query_graph.schema_by_source(&self.subgraph_name)?;
        let variable_usages = selection.used_variables()?;
        let mut operation = if self.is_entity_fetch {
            operation_for_entities_fetch(
                subgraph_schema,
                selection,
                variable_definitions,
                &operation_name,
            )?
        } else {
            operation_for_query_fetch(
                subgraph_schema,
                self.root_kind,
                selection,
                variable_definitions,
                &operation_name,
            )?
        };
        let fragments = fragments
            .map(|rebased| rebased.for_subgraph(self.subgraph_name.clone(), subgraph_schema));
        operation.optimize(fragments, Default::default());
        let operation_document = operation.try_into()?;

        let node = super::PlanNode::Fetch(Box::new(super::FetchNode {
            subgraph_name: self.subgraph_name.clone(),
            id: self.id,
            variable_usages,
            requires: input_nodes.map(|sel| executable::SelectionSet::from(sel).selections),
            operation_document,
            operation_name,
            operation_kind: self.root_kind.into(),
            input_rewrites: self.input_rewrites.clone(),
            output_rewrites,
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

    fn finalize_selection(
        &self,
        variable_definitions: &[Node<VariableDefinition>],
        handled_conditions: &Conditions,
        fragments: &Option<&mut RebasedFragments>,
    ) -> Result<(NormalizedSelectionSet, Vec<Arc<FetchDataRewrite>>), FederationError> {
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
            selection_without_conditions.add_typename_field_for_abstract_types(None, fragments)?;

        let (updated_selection, output_rewrites) =
            selection_with_typenames.add_aliases_for_non_merging_fields()?;

        updated_selection.validate(variable_definitions)?;
        Ok((updated_selection, output_rewrites))
    }
}

fn operation_for_entities_fetch(
    subgraph_schema: &ValidFederationSchema,
    selection_set: NormalizedSelectionSet,
    all_variable_definitions: &[Node<VariableDefinition>],
    operation_name: &Option<NodeStr>,
) -> Result<NormalizedOperation, FederationError> {
    let mut variable_definitions: Vec<Node<VariableDefinition>> =
        Vec::with_capacity(all_variable_definitions.len() + 1);
    variable_definitions.push(representations_variable_definition(subgraph_schema)?);
    let mut used_variables = HashSet::new();
    selection_set.collect_variables(&mut used_variables)?;
    variable_definitions.extend(
        all_variable_definitions
            .iter()
            .filter(|definition| used_variables.contains(&definition.name))
            .cloned(),
    );

    let query_type_name = subgraph_schema.schema().root_operation(OperationType::Query).ok_or_else(||
    FederationError::SingleFederationError(SingleFederationError::InvalidGraphQL {
        message: "Subgraphs should always have a query root (they should at least provides _entities)".to_string()
    }))?;

    let query_type = match subgraph_schema.get_type(query_type_name.clone())? {
        crate::schema::position::TypeDefinitionPosition::Object(o) => o,
        _ => {
            return Err(FederationError::SingleFederationError(
                SingleFederationError::InvalidGraphQL {
                    message: "the root query type must be an object".to_string(),
                },
            ))
        }
    };

    if !query_type
        .get(subgraph_schema.schema())?
        .fields
        .contains_key(&ENTITIES_QUERY)
    {
        return Err(FederationError::SingleFederationError(
            SingleFederationError::InvalidGraphQL {
                message: "Subgraphs should always have the _entities field".to_string(),
            },
        ));
    }

    let entities = FieldDefinitionPosition::Object(query_type.field(ENTITIES_QUERY.clone()));

    let entities_call = selection_of_element(
        OpPathElement::Field(NormalizedField::new(NormalizedFieldData {
            schema: subgraph_schema.clone(),
            field_position: entities,
            alias: None,
            arguments: Arc::new(vec![executable::Argument {
                name: FEDERATION_REPRESENTATIONS_ARGUMENTS_NAME,
                value: executable::Value::Variable(FEDERATION_REPRESENTATIONS_VAR_NAME).into(),
            }
            .into()]),
            directives: Default::default(),
            sibling_typename: None,
        })),
        Some(selection_set),
    )?;

    let type_position: CompositeTypeDefinitionPosition = subgraph_schema
        .get_type(query_type_name.clone())?
        .try_into()?;

    let mut map = NormalizedSelectionMap::new();
    map.insert(entities_call);

    let selection_set = NormalizedSelectionSet {
        schema: subgraph_schema.clone(),
        type_position,
        selections: Arc::new(map),
    };

    Ok(NormalizedOperation {
        schema: subgraph_schema.clone(),
        root_kind: SchemaRootDefinitionKind::Query,
        name: operation_name.clone().map(|n| n.try_into()).transpose()?,
        variables: Arc::new(variable_definitions),
        directives: Default::default(),
        selection_set,
        named_fragments: Default::default(),
    })
}

fn operation_for_query_fetch(
    subgraph_schema: &ValidFederationSchema,
    root_kind: SchemaRootDefinitionKind,
    selection_set: NormalizedSelectionSet,
    variable_definitions: &[Node<VariableDefinition>],
    operation_name: &Option<NodeStr>,
) -> Result<NormalizedOperation, FederationError> {
    let mut used_variables = HashSet::new();
    selection_set.collect_variables(&mut used_variables)?;
    let variable_definitions = variable_definitions
        .iter()
        .filter(|definition| used_variables.contains(&definition.name))
        .cloned()
        .collect();

    Ok(NormalizedOperation {
        schema: subgraph_schema.clone(),
        root_kind,
        name: operation_name.clone().map(|n| n.try_into()).transpose()?,
        variables: Arc::new(variable_definitions),
        directives: Default::default(),
        selection_set,
        named_fragments: Default::default(),
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

impl NormalizedSelectionSet {
    pub(crate) fn cost(&self, depth: QueryPlanCost) -> Result<QueryPlanCost, FederationError> {
        // The cost is essentially the number of elements in the selection,
        // but we make deep element cost a tiny bit more,
        // mostly to make things a tad more deterministic
        // (typically, if we have an interface with a single implementation,
        // then we can have a choice between a query plan that type-explode a field of the interface
        // and one that doesn't, and both will be almost identical,
        // except that the type-exploded field will be a different depth;
        // by favoring lesser depth in that case, we favor not type-exploding).
        self.selections.values().try_fold(0, |sum, selection| {
            let subselections = match selection {
                NormalizedSelection::Field(field) => field.selection_set.as_ref(),
                NormalizedSelection::InlineFragment(inline) => Some(&inline.selection_set),
                NormalizedSelection::FragmentSpread(_) => {
                    return Err(FederationError::internal(
                        "unexpected fragment spread in FetchDependencyGraphNode",
                    ))
                }
            };
            let subselections_cost = if let Some(selection_set) = subselections {
                selection_set.cost(depth + 1)?
            } else {
                0
            };
            Ok(sum + depth + subselections_cost)
        })
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

    fn add_at_path(
        &mut self,
        path_in_node: &OpPath,
        selection_set: Option<&Arc<NormalizedSelectionSet>>,
    ) -> Result<(), FederationError> {
        Arc::make_mut(&mut self.selection_set).add_at_path(path_in_node, selection_set);
        // TODO: when calling this multiple times, maybe only re-compute conditions at the end?
        // Or make it lazily-initialized and computed on demand?
        self.conditions = self.selection_set.conditions()?;
        Ok(())
    }
}

impl FetchInputs {
    pub(crate) fn empty(supergraph_schema: ValidFederationSchema) -> Self {
        Self {
            selection_sets_per_parent_type: Default::default(),
            supergraph_schema,
        }
    }

    fn add(&self, selection: &NormalizedSelectionSet) {
        assert_eq!(
            selection.schema, self.supergraph_schema,
            "Inputs selections must be based on the supergraph schema"
        );
        todo!()
    }

    fn to_selection_set_nodes(
        &self,
        variable_definitions: &[Node<VariableDefinition>],
        handled_conditions: &Conditions,
        type_position: &CompositeTypeDefinitionPosition,
    ) -> Result<NormalizedSelectionSet, FederationError> {
        let mut selections = NormalizedSelectionMap::new();
        for selection_set in self.selection_sets_per_parent_type.values() {
            let selection_set =
                remove_conditions_from_selection_set(selection_set, handled_conditions)?;
            // Making sure we're not generating something invalid.
            selection_set.validate(variable_definitions)?;
            selections.extend_ref(&selection_set.selections)
        }
        Ok(NormalizedSelectionSet {
            schema: self.supergraph_schema.clone(),
            type_position: type_position.clone(),
            selections: Arc::new(selections),
        })
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
            primary_selection: root_type_for_defer.map(|type_position| {
                Arc::new(NormalizedSelectionSet {
                    schema: schema.clone(),
                    type_position,
                    selections: Default::default(),
                })
            }),
        }
    }

    fn update_subselection(
        &mut self,
        defer_context: &DeferContext,
        selection_set: Option<&Arc<NormalizedSelectionSet>>,
    ) {
        if !defer_context.is_part_of_query {
            return;
        }
        let Some(primary_selection) = &mut self.primary_selection else {
            return;
        };
        if let Some(parent_ref) = &defer_context.current_defer_ref {
            self.deferred[parent_ref]
                .sub_selection
                .add_at_path(&defer_context.path_to_defer_parent, selection_set)
        } else {
            let primary_selection = Arc::make_mut(primary_selection);
            Arc::make_mut(&mut primary_selection.selections)
                .add_at_path(&defer_context.path_to_defer_parent, selection_set)
        }
    }
}

struct ComputeNodesStackItem<'a> {
    tree: &'a OpPathTree,
    node_id: NodeIndex,
    node_path: FetchDependencyGraphNodePath,
    context: &'a OpGraphPathContext,
    defer_context: DeferContext,
}

pub(crate) fn compute_nodes_for_tree(
    dependency_graph: &mut FetchDependencyGraph,
    initial_tree: &OpPathTree,
    initial_node_id: NodeIndex,
    initial_node_path: FetchDependencyGraphNodePath,
    initial_defer_context: DeferContext,
    initial_conditions: &OpGraphPathContext,
) -> Result<IndexSet<NodeIndex>, FederationError> {
    let mut stack = vec![ComputeNodesStackItem {
        tree: initial_tree,
        node_id: initial_node_id,
        node_path: initial_node_path,
        context: initial_conditions,
        defer_context: initial_defer_context,
    }];
    let mut created_nodes = IndexSet::new();
    while let Some(stack_item) = stack.pop() {
        let node =
            FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
        for selection_set in &stack_item.tree.local_selection_sets {
            node.selection_set_mut()
                .add_at_path(&stack_item.node_path.path_in_node, Some(selection_set))?;
            dependency_graph
                .defer_tracking
                .update_subselection(&stack_item.defer_context, Some(selection_set));
        }
        if stack_item.tree.is_leaf() {
            node.selection_set_mut()
                .add_at_path(&stack_item.node_path.path_in_node, None)?;
            dependency_graph
                .defer_tracking
                .update_subselection(&stack_item.defer_context, None);
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
                            )))
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
                    )?);
                }
            }
        }
    }
    Ok(created_nodes)
}

fn compute_nodes_for_key_resolution<'a>(
    dependency_graph: &mut FetchDependencyGraph,
    stack_item: &ComputeNodesStackItem<'a>,
    child: &'a PathTreeChild<OpGraphPathTrigger, Option<EdgeIndex>>,
    edge_id: EdgeIndex,
    new_context: &'a OpGraphPathContext,
    created_nodes: &mut IndexSet<NodeIndex>,
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
        stack_item.defer_context.clone(),
        &Default::default(),
    )?;
    created_nodes.extend(conditions_nodes.iter().copied());
    // Then we can "take the edge", creating a new node.
    // That node depends on the condition ones.
    let (source_id, dest_id) = stack_item.tree.graph.edge_endpoints(edge_id)?;
    let source = stack_item.tree.graph.node_weight(source_id)?;
    let dest = stack_item.tree.graph.node_weight(dest_id)?;
    // We shouldn't have a key on a non-composite type
    let source_type: CompositeTypeDefinitionPosition = source.type_.clone().try_into()?;
    let dest_type: CompositeTypeDefinitionPosition = dest.type_.clone().try_into()?;
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
            path_in_parent: Some(path_in_parent),
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
        let mut path = None;
        let mut iter = dependency_graph.parents_relations_of(condition_node);
        if let (Some(condition_node_parent), None) = (iter.next(), iter.next()) {
            // There is exactly one parent
            if condition_node_parent.parent_node_id == stack_item.node_id {
                if let Some(condition_path) = condition_node_parent.path_in_parent {
                    path = condition_path.strip_prefix(path_in_parent).map(Arc::new)
                }
            }
        }
        drop(iter);
        dependency_graph.add_parent(
            new_node_id,
            ParentRelation {
                parent_node_id: condition_node,
                path_in_parent: path.as_ref(),
            },
        )
    }
    // Note that inputs must be based on the supergraph schema, not any particular subgraph,
    // since sometimes key conditions are fetched from multiple subgraphs
    // (and so no one subgraph has a type definition with all the proper fields,
    // only the supergraph does).
    let input_type = dependency_graph.type_for_fetch_inputs(source_type.type_name())?;
    let mut input_selections = NormalizedSelectionSet::empty(
        dependency_graph.supergraph_schema.clone(),
        input_type.clone(),
    );
    let Some(edge_conditions) = &edge.conditions else {
        // PORT_NOTE: TypeScript `computeGroupsForTree()` has a non-null assertion here
        return Err(FederationError::internal(
            "missing expected edge conditions",
        ));
    };
    input_selections.add(edge_conditions);
    let new_node =
        &mut FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, new_node_id)?;
    new_node.add_inputs(
        &dependency_graph.supergraph_schema,
        &wrap_input_selections(&input_type, &input_selections, new_context),
        compute_input_rewrites_on_key_fetch(input_type.type_name(), &dest_type)
            .into_iter()
            .flatten(),
    );

    // We also ensure to get the __typename of the current type in the "original" node.
    let node =
        FetchDependencyGraph::node_weight_mut(&mut dependency_graph.graph, stack_item.node_id)?;
    let typename_field = Arc::new(OpPathElement::Field(NormalizedField::new(
        NormalizedFieldData {
            schema: dependency_graph.supergraph_schema.clone(),
            field_position: source_type.introspection_typename_field(),
            alias: None,
            arguments: Default::default(),
            directives: Default::default(),
            sibling_typename: None,
        },
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
            )),
        context: new_context,
        defer_context: updated_defer_context,
    })
}

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
        let typename_field = Arc::new(OpPathElement::Field(NormalizedField::new(
            NormalizedFieldData {
                schema: dependency_graph.supergraph_schema.clone(),
                field_position: source_type.introspection_typename_field().into(),
                alias: None,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            },
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
            path_in_parent: Some(&stack_item.node_path.path_in_node),
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
            )),

        context: new_context,
        defer_context: updated_defer_context,
    })
}

fn compute_nodes_for_op_path_element<'a>(
    dependency_graph: &mut FetchDependencyGraph,
    stack_item: &ComputeNodesStackItem<'a>,
    child: &'a Arc<PathTreeChild<OpGraphPathTrigger, Option<EdgeIndex>>>,
    operation: &OpPathElement,
    created_nodes: &mut IndexSet<NodeIndex>,
) -> Result<ComputeNodesStackItem<'a>, FederationError> {
    let Some(edge_id) = child.edge else {
        // A null edge means that the operation does nothing
        // but may contain directives to preserve.
        // If it does contains directives, we look for @defer in particular.
        // If we find it, this means that we should change our current node
        // to one for the defer in question.
        let (updated_operation, updated_defer_context) = extract_defer_from_operation(
            dependency_graph,
            operation,
            &stack_item.defer_context,
            &stack_item.node_path,
        );
        // We've now removed any @defer.
        // If the operation contains other directives or a non-trivial type condition,
        // we need to preserve it and so we add operation.
        // Otherwise, we just skip it as a minor optimization (it makes the subgraph query
        // slighly smaller and on complex queries, it might also deduplicate similar selections).
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
        });
    };
    let (source_id, dest_id) = stack_item.tree.graph.edge_endpoints(edge_id)?;
    let source = stack_item.tree.graph.node_weight(source_id)?;
    let dest = stack_item.tree.graph.node_weight(dest_id)?;
    if source.source != dest.source {
        return Err(FederationError::internal(format!(
            "Collecting edge {edge_id:?} for {operation:?} \
                                 should not change the underlying subgraph"
        )));
    }

    // We have a operation element, field or inline fragment.
    // We first check if it's been "tagged" to remember that __typename must be queried.
    // See the comment on the `optimize_sibling_typenames()` method to see why this exists.
    if let Some(name) = operation.sibling_typename() {
        // We need to add the query __typename for the current type in the current node.
        // Note that `name` is the alias or '' if there is no alias
        let alias = if name.is_empty() {
            None
        } else {
            Some(name.clone())
        };
        let typename_field = Arc::new(OpPathElement::Field(NormalizedField::new(
            NormalizedFieldData {
                schema: dependency_graph.supergraph_schema.clone(),
                field_position: operation
                    .parent_type_position()
                    .introspection_typename_field(),
                alias,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            },
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
        )
    }
    let (Some(updated_operation), updated_defer_context) = extract_defer_from_operation(
        dependency_graph,
        operation,
        &stack_item.defer_context,
        &stack_item.node_path,
    ) else {
        return Err(FederationError::internal(format!(
            "Extracting @defer from {operation:?} should not have resulted in no operation"
        )));
    };
    let mut updated = ComputeNodesStackItem {
        tree: &child.tree,
        node_id: stack_item.node_id,
        node_path: stack_item.node_path.clone(),
        context: stack_item.context,
        defer_context: updated_defer_context,
    };
    if let Some(conditions) = &child.conditions {
        // We have @requires or some other dependency to create nodes for.
        let (required_node_id, require_path) = handle_requires(
            dependency_graph,
            edge_id,
            conditions,
            (stack_item.node_id, &stack_item.node_path),
            stack_item.context,
            &updated.defer_context,
            created_nodes,
        )?;
        updated.node_id = required_node_id;
        updated.node_path = require_path;
    }
    if let OpPathElement::Field(field) = &updated_operation {
        if *field.data().name() == TYPENAME_FIELD {
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
            let updated_node = FetchDependencyGraph::node_weight_mut(
                &mut dependency_graph.graph,
                updated.node_id,
            )?;
            updated_node.must_preserve_selection_set = true
        }
    }
    let edge = child.tree.graph.edge_weight(edge_id)?;
    if let QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. } = &edge.transition {
        // We shouldn't add the operation "as is" as it's a down-cast but we're "faking it".
        // However, if the operation has directives, we should preserve that.
        let OpPathElement::InlineFragment(inline) = updated_operation else {
            return Err(FederationError::internal(format!(
                "Unexpected operation {updated_operation} for edge {edge}"
            )));
        };
        if !inline.data().directives.is_empty() {
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

fn wrap_input_selections(
    _wrapping_type: &CompositeTypeDefinitionPosition,
    _selections: &NormalizedSelectionSet,
    _context: &OpGraphPathContext,
) -> NormalizedSelectionSet {
    todo!() // Port `wrapInputsSelections` in `buildPlan.ts`
}

fn compute_input_rewrites_on_key_fetch(
    _input_type_name: &str,
    _dest_type: &CompositeTypeDefinitionPosition,
) -> Option<Vec<Arc<FetchDataRewrite>>> {
    todo!() // Port `computeInputRewritesOnKeyFetch`
}

fn create_fetch_initial_path(
    _supergraph_schema: &ValidFederationSchema,
    _dest_type: &CompositeTypeDefinitionPosition,
    _new_context: &OpGraphPathContext,
) -> Arc<OpPath> {
    todo!() // Port `createFetchInitialPath`
}

fn extract_defer_from_operation(
    _dependency_graph: &mut FetchDependencyGraph,
    _operation: &OpPathElement,
    _defer_context: &DeferContext,
    _node_path: &FetchDependencyGraphNodePath,
) -> (Option<OpPathElement>, DeferContext) {
    todo!() // Port `extractDeferFromOperation`
}

fn handle_requires(
    _dependency_graph: &mut FetchDependencyGraph,
    _edge_id: EdgeIndex,
    _requires_conditions: &OpPathTree,
    (_node_id, _node_path): (NodeIndex, &FetchDependencyGraphNodePath),
    _context: &OpGraphPathContext,
    _defer_context: &DeferContext,
    _created_nodes: &mut IndexSet<NodeIndex>,
) -> Result<(NodeIndex, FetchDependencyGraphNodePath), FederationError> {
    // PORT_NOTE: instead of returing IDs of created nodes they should be inserted directly
    // in the `created_nodes` set passed by mutable reference.
    todo!() // Port `handleRequires`
}
