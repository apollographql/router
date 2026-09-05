use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use indexmap::map::Entry;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use serde::Serialize;

use super::graph_path::ArgumentsToContextUsages;
use super::graph_path::MatchingContextIds;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphNode;
use crate::query_graph::graph_path::GraphPathItem;
use crate::query_graph::graph_path::operation::OpGraphPath;
use crate::query_graph::graph_path::operation::OpGraphPathTrigger;
use crate::utils::FallibleIterator;

/// A "merged" tree representation for a vector of `GraphPath`s that start at a common query graph
/// node, in which each node of the tree corresponds to a node in the query graph, and a tree's node
/// has a child for every unique pair of edge and trigger.
// PORT_NOTE: The JS codebase additionally has a property `triggerEquality`; this existed because
// Typescript doesn't have a native way of associating equality/hash functions with types, so they
// were passed around manually. This isn't the case with Rust, where we instead implement trigger
// equality via `PartialEq` and `Hash`.
#[derive(Serialize)]
pub(crate) struct PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    /// The query graph of which this is a path tree.
    // TODO: This is probably useful information for snapshot logging, but it can probably be
    // inferred by the visualizer
    #[serde(skip)]
    pub(crate) graph: Arc<QueryGraph>,
    /// The query graph node at which the path tree starts.
    pub(crate) node: NodeIndex,
    /// Note that `ClosedPath`s have an optimization which splits them into paths and a selection
    /// set representing a trailing query to a single subgraph at the final nodes of the paths. For
    /// such paths where this `PathTree`'s node corresponds to that final node, those selection sets
    /// are collected here. This is really an optimization to avoid unnecessary merging of selection
    /// sets when they query a single subgraph.
    pub(crate) local_selection_sets: Vec<Arc<SelectionSet>>,
    /// The child `PathTree`s for this `PathTree` node. There is a child for every unique pair of
    /// edge and trigger present at this particular sub-path within the `GraphPath`s covered by this
    /// `PathTree` node.
    pub(crate) childs: Vec<Arc<PathTreeChild<TTrigger, TEdge>>>,
}

impl<TTrigger, TEdge> Clone for PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            node: self.node,
            local_selection_sets: self.local_selection_sets.clone(),
            childs: self.childs.clone(),
        }
    }
}

impl<TTrigger, TEdge> PartialEq for PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + PartialEq + Into<Option<EdgeIndex>>,
{
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.graph, &other.graph)
            && self.node == other.node
            && self.local_selection_sets == other.local_selection_sets
            && self.childs == other.childs
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PathTreeChild<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    /// The edge connecting this child to its parent.
    pub(crate) edge: TEdge,
    /// The trigger for the edge connecting this child to its parent.
    pub(crate) trigger: Arc<TTrigger>,
    /// The conditions required to be fetched if this edge is taken.
    pub(crate) conditions: Option<Arc<OpPathTree>>,
    /// The child `PathTree` reached by taking the edge.
    pub(crate) tree: Arc<PathTree<TTrigger, TEdge>>,
    // PORT_NOTE: This field was renamed because the JS name (`contextToSelection`) implied it was
    // a map to selections, which it isn't.
    /// The IDs of contexts that have matched at the edge.
    pub(crate) matching_context_ids: Option<MatchingContextIds>,
    // PORT_NOTE: This field was renamed because the JS name (`parameterToContext`) left confusion
    // to how a parameter was different from an argument.
    /// A map of @fromContext arguments to info about the contexts used in those arguments.
    pub(crate) arguments_to_context_usages: Option<ArgumentsToContextUsages>,
}

impl<TTrigger, TEdge> PartialEq for PathTreeChild<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + PartialEq + Into<Option<EdgeIndex>>,
{
    fn eq(&self, other: &Self) -> bool {
        self.edge == other.edge
            && self.trigger == other.trigger
            && self.conditions == other.conditions
            && self.tree == other.tree
            && self.matching_context_ids == other.matching_context_ids
            && self.arguments_to_context_usages == other.arguments_to_context_usages
    }
}

/// A `PathTree` whose triggers are operation elements (essentially meaning that the constituent
/// `GraphPath`s were guided by a GraphQL operation).
pub(crate) type OpPathTree = PathTree<OpGraphPathTrigger, Option<EdgeIndex>>;

impl OpPathTree {
    pub(crate) fn new(graph: Arc<QueryGraph>, node: NodeIndex) -> Self {
        Self {
            graph,
            node,
            local_selection_sets: Vec::new(),
            childs: Vec::new(),
        }
    }

    pub(crate) fn from_op_paths(
        graph: Arc<QueryGraph>,
        node: NodeIndex,
        paths: &[(&OpGraphPath, Option<&Arc<SelectionSet>>)],
    ) -> Result<Self, FederationError> {
        assert!(
            !paths.is_empty(),
            "OpPathTree cannot be created from an empty set of paths"
        );
        Self::from_paths(
            graph,
            node,
            paths
                .iter()
                .map(|(path, selections)| (path.iter(), *selections))
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) fn is_leaf(&self) -> bool {
        self.childs.is_empty()
    }

    pub(crate) fn is_all_in_same_subgraph(&self) -> Result<bool, FederationError> {
        let node_weight = self.graph.node_weight(self.node)?;
        self.is_all_in_same_subgraph_internal(&node_weight.source)
    }

    fn is_all_in_same_subgraph_internal(&self, target: &Arc<str>) -> Result<bool, FederationError> {
        let node_weight = self.graph.node_weight(self.node)?;
        if node_weight.source != *target {
            return Ok(false);
        }
        self.childs
            .iter()
            .fallible_all(|child| child.tree.is_all_in_same_subgraph_internal(target))
    }

    fn fmt_internal(
        &self,
        f: &mut Formatter<'_>,
        indent: &str,
        include_conditions: bool,
    ) -> std::fmt::Result {
        if self.is_leaf() {
            return write!(f, "{}", self.vertex());
        }
        write!(f, "{}:", self.vertex())?;
        let child_indent = format!("{indent}  ");
        for child in self.childs.iter() {
            let index = child.edge.unwrap_or_else(EdgeIndex::end);
            write!(f, "\n{indent} -> [{}] ", index.index())?;
            if include_conditions && let Some(ref child_cond) = child.conditions {
                write!(f, "!! {{\n{indent} ")?;
                child_cond.fmt_internal(f, &child_indent, /*include_conditions*/ true)?;
                write!(f, "\n{indent} }}")?;
            }
            write!(f, "{} = ", child.trigger)?;
            child
                .tree
                .fmt_internal(f, &child_indent, include_conditions)?;
        }
        Ok(())
    }
}

impl Display for OpPathTree {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let indent = "".to_owned(); // Empty indent at the root level
        self.fmt_internal(f, &indent, /*include_conditions*/ false)
    }
}

/// A partial ordering over type `T` in terms of preference.
/// - Similar to PartialOrd, but equivalence is unnecessary.
pub(crate) trait Preference {
    /// - Returns None, if `self` and `other` are incomparable or equivalent.
    /// - Returns Some(true), if `self` is preferred over `other`.
    /// - Returns Some(false), if `other` is preferred over `self`.
    fn preferred_over(&self, other: &Self) -> Option<bool>;
}

impl<TTrigger, TEdge> PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash + Preference,
    TEdge: Copy + Hash + Eq + Into<Option<EdgeIndex>>,
{
    /// Returns the `QueryGraphNode` represented by `self.node`.
    /// PORT_NOTE: This is named after the JS implementation's `vertex` field.
    ///            But, it may make sense to rename it once porting is over.
    pub(crate) fn vertex(&self) -> &QueryGraphNode {
        self.graph.node_weight(self.node).unwrap()
    }

    fn from_paths<'inputs>(
        graph: Arc<QueryGraph>,
        node: NodeIndex,
        graph_paths_and_selections: Vec<(
            impl Iterator<Item = GraphPathItem<'inputs, TTrigger, TEdge>>,
            Option<&'inputs Arc<SelectionSet>>,
        )>,
    ) -> Result<Self, FederationError>
    where
        TTrigger: 'inputs,
        TEdge: 'inputs,
    {
        // Group by and order by unique edge ID, and among those by unique trigger
        let mut merged =
            IndexMap::<TEdge, ByUniqueEdge<TTrigger, /* impl Iterator */ _>>::default();

        struct ByUniqueEdge<'inputs, TTrigger, GraphPathIter> {
            target_node: NodeIndex,
            by_unique_trigger: IndexMap<
                &'inputs Arc<TTrigger>,
                PathTreeChildInputs<'inputs, TTrigger, GraphPathIter>,
            >,
        }

        struct PathTreeChildInputs<'inputs, TTrigger, GraphPathIter> {
            /// trigger: the final trigger value chosen amongst the candidate triggers
            ///   - Two equivalent triggers can have minor differences in the sibling_typename.
            ///     This field holds the final trigger value that will be used.
            ///
            /// PORT_NOTE: The JS QP used the last trigger value, since the next trigger value
            ///            overwrites the `trigger` field. Instead, Rust QP adopts the one with the
            ///            sibling_typename set or the first one if none are set.
            trigger: &'inputs Arc<TTrigger>,
            conditions: Option<Arc<OpPathTree>>,
            sub_paths_and_selections: Vec<(GraphPathIter, Option<&'inputs Arc<SelectionSet>>)>,
            matching_context_ids: Option<MatchingContextIds>,
            arguments_to_context_usages: Option<ArgumentsToContextUsages>,
        }

        let mut local_selection_sets = Vec::new();

        for (mut graph_path_iter, selection) in graph_paths_and_selections {
            let Some((
                generic_edge,
                trigger,
                conditions,
                matching_context_ids,
                arguments_to_context_usages,
            )) = graph_path_iter.next()
            else {
                // End of an input `GraphPath`
                if let Some(selection) = selection {
                    local_selection_sets.push(selection.clone());
                }
                continue;
            };
            let for_edge = match merged.entry(generic_edge) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    entry.insert(ByUniqueEdge {
                        target_node: if let Some(edge) = generic_edge.into() {
                            let (_source, target) = graph.edge_endpoints(edge)?;
                            target
                        } else {
                            // For a "None" edge, stay on the same node
                            node
                        },
                        by_unique_trigger: IndexMap::default(),
                    })
                }
            };
            match for_edge.by_unique_trigger.entry(trigger) {
                Entry::Occupied(entry) => {
                    let existing = entry.into_mut();
                    if trigger.preferred_over(existing.trigger) == Some(true) {
                        existing.trigger = trigger;
                    }
                    existing.conditions = merge_conditions(&existing.conditions, conditions);
                    if let Some(other) = matching_context_ids {
                        existing
                            .matching_context_ids
                            .get_or_insert_with(Default::default)
                            .extend(other.iter().cloned());
                    }
                    if let Some(other) = arguments_to_context_usages {
                        existing
                            .arguments_to_context_usages
                            .get_or_insert_with(Default::default)
                            .extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                    existing
                        .sub_paths_and_selections
                        .push((graph_path_iter, selection))
                    // Note that as we merge, we don't create a new child
                }
                Entry::Vacant(entry) => {
                    entry.insert(PathTreeChildInputs {
                        trigger,
                        conditions: conditions.clone(),
                        sub_paths_and_selections: vec![(graph_path_iter, selection)],
                        matching_context_ids: matching_context_ids.cloned(),
                        arguments_to_context_usages: arguments_to_context_usages.cloned(),
                    });
                }
            }
        }

        let mut childs = Vec::new();
        for (edge, by_unique_edge) in merged {
            for (_, child) in by_unique_edge.by_unique_trigger {
                childs.push(Arc::new(PathTreeChild {
                    edge,
                    trigger: child.trigger.clone(),
                    conditions: child.conditions.clone(),
                    tree: Arc::new(Self::from_paths(
                        graph.clone(),
                        by_unique_edge.target_node,
                        child.sub_paths_and_selections,
                    )?),
                    matching_context_ids: child.matching_context_ids.clone(),
                    arguments_to_context_usages: child.arguments_to_context_usages.clone(),
                }))
            }
        }
        Ok(Self {
            graph,
            node,
            local_selection_sets,
            childs,
        })
    }

    fn merge_if_not_equal(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
        if self.equals_same_root(other) {
            self.clone()
        } else {
            self.merge(other)
        }
    }

    /// May have false negatives (see comment about `Arc::ptr_eq`)
    pub(crate) fn equals_same_root(self: &Arc<Self>, other: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, other)
            || self.childs.len() == other.childs.len()
                && self.childs.iter().zip(&other.childs).all(|(a, b)| {
                a.edge == b.edge
                    // `Arc::ptr_eq` instead of `==` is faster and good enough.
                    // This method is all about avoid unnecessary merging
                    // when we suspect conditions trees have been build from the exact same inputs.
                    && Arc::ptr_eq(&a.trigger, &b.trigger)
                    && match (&a.conditions, &b.conditions) {
                        (None, None) => true,
                        (Some(cond_a), Some(cond_b)) => cond_a.equals_same_root(cond_b),
                        _ => false,
                    }
                    && match (&a.matching_context_ids, &b.matching_context_ids) {
                        (Some(_), Some(_)) => a.matching_context_ids == b.matching_context_ids,
                        (_, _) =>
                            a.matching_context_ids.as_ref().map(|c| c.is_empty()).unwrap_or(true) &&
                                b.matching_context_ids.as_ref().map(|c| c.is_empty()).unwrap_or(true)
                    }
                    && match (&a.arguments_to_context_usages, &b.arguments_to_context_usages) {
                        (Some(_), Some(_)) => a.arguments_to_context_usages == b.arguments_to_context_usages,
                        (_, _) =>
                            a.arguments_to_context_usages.as_ref().map(|c| c.is_empty()).unwrap_or(true) &&
                                b.arguments_to_context_usages.as_ref().map(|c| c.is_empty()).unwrap_or(true)
                    }
                    && a.tree.equals_same_root(&b.tree)
            })
    }

    /// Appends the children of the other `OpTree` onto the children of this tree.
    ///
    /// ## Panics
    /// Like `Self::merge`, this method will panic if the graphs of the two `OpTree`s below to
    /// different allocations (i.e. they don't below to the same graph) or if they below to
    /// different root nodes.
    pub(crate) fn extend(&mut self, other: &Self) {
        assert!(
            Arc::ptr_eq(&self.graph, &other.graph),
            "Cannot merge path tree build on another graph"
        );
        assert_eq!(
            self.node, other.node,
            "Cannot merge path trees rooted different nodes"
        );
        if other.childs.is_empty() {
            return;
        }
        if self.childs.is_empty() {
            self.clone_from(other);
            return;
        }
        self.childs.extend_from_slice(&other.childs);
        self.local_selection_sets
            .extend_from_slice(&other.local_selection_sets);
    }

    /// ## Panics
    /// This method will panic if the graphs of the two `OpTree`s below to different allocations
    /// (i.e. they don't below to the same graph) or if they below to different root nodes.
    pub(crate) fn merge(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
        if Arc::ptr_eq(self, other) {
            return self.clone();
        }
        assert!(
            Arc::ptr_eq(&self.graph, &other.graph),
            "Cannot merge path tree build on another graph"
        );
        assert_eq!(
            self.node, other.node,
            "Cannot merge path trees rooted different nodes"
        );
        if other.childs.is_empty() {
            return self.clone();
        }
        if self.childs.is_empty() {
            return other.clone();
        }

        let mut count_to_add = 0;
        let merge_indices: Vec<_> = other
            .childs
            .iter()
            .map(|other_child| {
                let position = self.childs.iter().position(|self_child| {
                    self_child.edge == other_child.edge && self_child.trigger == other_child.trigger
                });
                if position.is_none() {
                    count_to_add += 1
                }
                position
            })
            .collect();
        let expected_new_len = self.childs.len() + count_to_add;
        let mut childs = Vec::with_capacity(expected_new_len);
        childs.extend(self.childs.iter().cloned());
        for (other_child, merge_index) in other.childs.iter().zip(merge_indices) {
            if let Some(i) = merge_index {
                let child = &mut childs[i];
                *child = Arc::new(PathTreeChild {
                    edge: child.edge,
                    trigger: child.trigger.clone(),
                    conditions: merge_conditions(&child.conditions, &other_child.conditions),
                    tree: child.tree.merge(&other_child.tree),
                    matching_context_ids: merge_matching_context_ids(
                        &child.matching_context_ids,
                        &other_child.matching_context_ids,
                    ),
                    arguments_to_context_usages: merge_arguments_to_context_usages(
                        &child.arguments_to_context_usages,
                        &other_child.arguments_to_context_usages,
                    ),
                })
            } else {
                childs.push(other_child.clone())
            }
        }
        assert_eq!(childs.len(), expected_new_len);

        Arc::new(Self {
            graph: self.graph.clone(),
            node: self.node,
            local_selection_sets: self
                .local_selection_sets
                .iter()
                .chain(&other.local_selection_sets)
                .cloned()
                .collect(),
            childs,
        })
    }
}

fn merge_matching_context_ids(
    a: &Option<MatchingContextIds>,
    b: &Option<MatchingContextIds>,
) -> Option<MatchingContextIds> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let mut merged: MatchingContextIds = Default::default();
            merged.extend(a.iter().cloned());
            merged.extend(b.iter().cloned());
            Some(merged)
        }
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

fn merge_arguments_to_context_usages(
    a: &Option<ArgumentsToContextUsages>,
    b: &Option<ArgumentsToContextUsages>,
) -> Option<ArgumentsToContextUsages> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let mut merged: ArgumentsToContextUsages = Default::default();
            merged.extend(a.iter().map(|(k, v)| (k.clone(), v.clone())));
            merged.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
            Some(merged)
        }
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

fn merge_conditions(
    a: &Option<Arc<OpPathTree>>,
    b: &Option<Arc<OpPathTree>>,
) -> Option<Arc<OpPathTree>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.merge_if_not_equal(b)),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

impl<TTrigger: std::fmt::Debug, TEdge: std::fmt::Debug> std::fmt::Debug
    for PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let Self {
            graph: _, // skip
            node,
            local_selection_sets,
            childs,
        } = self;
        f.debug_struct("PathTree")
            .field("node", node)
            .field("local_selection_sets", local_selection_sets)
            .field("childs", childs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use apollo_compiler::ast::Type;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::collections::IndexSet;
    use apollo_compiler::parser::Parser;
    use petgraph::stable_graph::EdgeIndex;
    use petgraph::stable_graph::NodeIndex;
    use petgraph::visit::EdgeRef;
    use proptest::prelude::*;
    use proptest::proptest;

    use crate::Supergraph;
    use crate::error::FederationError;
    use crate::operation::Field;
    use crate::operation::InlineFragment;
    use crate::operation::SelectionId;
    use crate::operation::SelectionSet;
    use crate::operation::never_cancel;
    use crate::operation::normalize_operation;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::query_graph::QueryGraphNodeType;
    use crate::query_graph::build_federated_query_graph;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::query_graph::condition_resolver::ConditionResolution;
    use crate::query_graph::graph_path::ContextUsageEntry;
    use crate::query_graph::graph_path::operation::OpGraphPath;
    use crate::query_graph::graph_path::operation::OpGraphPathContext;
    use crate::query_graph::graph_path::operation::OpGraphPathTrigger;
    use crate::query_graph::graph_path::operation::OpPathElement;
    use crate::query_graph::path_tree::OpPathTree;
    use crate::query_graph::path_tree::PathTreeChild;
    use crate::query_plan::FetchDataPathElement;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::CompositeTypeDefinitionPosition;
    use crate::schema::position::OutputTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;

    // NB: stole from operation.rs
    fn parse_schema_and_operation(
        schema_and_operation: &str,
    ) -> (ValidFederationSchema, ExecutableDocument) {
        let (schema, executable_document) = Parser::new()
            .parse_mixed_validate(schema_and_operation, "document.graphql")
            .unwrap();
        let executable_document = executable_document.into_inner();
        let schema = ValidFederationSchema::new(schema).unwrap();
        (schema, executable_document)
    }

    fn trivial_condition() -> ConditionResolution {
        ConditionResolution::Satisfied {
            cost: 0.0,
            path_tree: None,
            context_map: None,
        }
    }

    // A helper function that builds a graph path from a sequence of field names
    fn build_graph_path(
        query_graph: &Arc<QueryGraph>,
        op_kind: SchemaRootDefinitionKind,
        path: &[&str],
    ) -> Result<OpGraphPath, FederationError> {
        let nodes_by_kind = query_graph.root_kinds_to_nodes()?;
        let root_node_idx = nodes_by_kind[&op_kind];
        let mut graph_path = OpGraphPath::new(query_graph.clone(), root_node_idx)?;
        let mut curr_node_idx = root_node_idx;
        for field_name in path.iter() {
            // find the edge that matches `field_name`
            let (edge_ref, field_def) = query_graph
                .out_edges(curr_node_idx)
                .into_iter()
                .find_map(|e_ref| {
                    let edge = e_ref.weight();
                    match &edge.transition {
                        QueryGraphEdgeTransition::FieldCollection {
                            field_definition_position,
                            ..
                        } => {
                            if field_definition_position.field_name() == *field_name {
                                Some((e_ref, field_definition_position))
                            } else {
                                None
                            }
                        }

                        _ => None,
                    }
                })
                .unwrap();

            // build the trigger for the edge
            let field = Field {
                schema: query_graph.schema().unwrap().clone(),
                field_position: field_def.clone(),
                alias: None,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            };
            let trigger = OpGraphPathTrigger::OpPathElement(OpPathElement::Field(field));

            // add the edge to the path
            graph_path = graph_path
                .add(trigger, Some(edge_ref.id()), trivial_condition(), None)
                .unwrap();

            // prepare for the next iteration
            curr_node_idx = edge_ref.target();
        }
        Ok(graph_path)
    }

    fn path_tree_fixture() -> (Arc<QueryGraph>, NodeIndex) {
        let schema = Schema::parse_and_validate(
            r#"
            interface Node {
              id: ID!
              nested: Node
            }

            type A implements Node {
              id: ID!
              nested: Node
              a: String
            }

            type B implements Node {
              id: ID!
              nested: Node
              b: Int
            }

            union SearchResult = A | B

            type Query {
              node: Node
              search: SearchResult
              a: A
              b: B
            }
            "#,
            "path-tree-proptest.graphql",
        )
        .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        let graph =
            Arc::new(build_query_graph("path-tree".into(), schema, Default::default()).unwrap());
        let root = graph.root_kinds_to_nodes().unwrap()[&SchemaRootDefinitionKind::Query];
        (graph, root)
    }

    fn generated_trigger(
        graph: &QueryGraph,
        edge_id: EdgeIndex,
        variant: u8,
    ) -> OpGraphPathTrigger {
        match &graph.graph[edge_id].transition {
            QueryGraphEdgeTransition::FieldCollection {
                field_definition_position,
                ..
            } => {
                let mut field = Field::from_position(
                    graph.schema().unwrap(),
                    field_definition_position.clone(),
                );
                if variant % 3 != 0 {
                    field.alias = Some(
                        Name::new(format!("generated_alias_{}", variant % 3).as_str()).unwrap(),
                    );
                }
                field.into()
            }
            QueryGraphEdgeTransition::Downcast {
                from_type_position,
                to_type_position,
                ..
            } => InlineFragment {
                schema: graph.schema().unwrap().clone(),
                parent_type_position: from_type_position.clone(),
                type_condition_position: Some(to_type_position.clone()),
                directives: Default::default(),
                selection_id: SelectionId::new(),
            }
            .into(),
            QueryGraphEdgeTransition::KeyResolution
            | QueryGraphEdgeTransition::RootTypeResolution { .. }
            | QueryGraphEdgeTransition::SubgraphEnteringTransition
            | QueryGraphEdgeTransition::InterfaceObjectFakeDownCast { .. } => {
                OpGraphPathContext::default().into()
            }
        }
    }

    fn generated_graph_path(
        graph: &Arc<QueryGraph>,
        root: NodeIndex,
        commands: &[(u16, u8)],
    ) -> OpGraphPath {
        let mut path = OpGraphPath::new(graph.clone(), root).unwrap();
        let mut tail = root;
        for &(edge_seed, trigger_variant) in commands {
            let candidates = graph.out_edges(tail);
            if candidates.is_empty() {
                break;
            }
            let edge = candidates[edge_seed as usize % candidates.len()];
            let trigger = generated_trigger(graph, edge.id(), trigger_variant);
            path = path
                .add(trigger, Some(edge.id()), trivial_condition(), None)
                .unwrap();
            tail = edge.target();
        }
        path
    }

    fn trigger_signature(trigger: &OpGraphPathTrigger) -> String {
        match trigger {
            OpGraphPathTrigger::OpPathElement(OpPathElement::Field(field)) => format!(
                "field:{}:{}",
                field.field_position,
                field.alias.as_ref().map(Name::as_str).unwrap_or("")
            ),
            OpGraphPathTrigger::OpPathElement(OpPathElement::InlineFragment(fragment)) => format!(
                "fragment:{}:{}",
                fragment.parent_type_position,
                fragment
                    .type_condition_position
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            ),
            OpGraphPathTrigger::Context(context) => format!("context:{context}"),
        }
    }

    type SemanticBranch = Vec<(Option<usize>, String)>;

    fn graph_path_branch(path: &OpGraphPath) -> SemanticBranch {
        path.iter()
            .map(|(edge, trigger, _, _, _)| {
                (edge.map(EdgeIndex::index), trigger_signature(trigger))
            })
            .collect()
    }

    fn maximal_path_branches(paths: &[OpGraphPath]) -> BTreeSet<SemanticBranch> {
        let branches: BTreeSet<_> = paths.iter().map(graph_path_branch).collect();
        branches
            .iter()
            .filter(|branch| {
                !branches.iter().any(|other| {
                    other.len() > branch.len() && other.iter().take(branch.len()).eq(branch.iter())
                })
            })
            .cloned()
            .collect()
    }

    fn audit_and_collect_tree_branches(tree: &OpPathTree) -> BTreeSet<SemanticBranch> {
        fn visit(
            tree: &OpPathTree,
            prefix: &mut SemanticBranch,
            output: &mut BTreeSet<SemanticBranch>,
        ) {
            assert!(tree.local_selection_sets.is_empty());
            let mut unique_children = BTreeSet::new();
            for child in &tree.childs {
                let key = (
                    child.edge.map(EdgeIndex::index),
                    trigger_signature(&child.trigger),
                );
                assert!(
                    unique_children.insert(key.clone()),
                    "duplicate child for the same edge and trigger at node {:?}",
                    tree.node
                );
                let expected_target = child
                    .edge
                    .map(|edge| tree.graph.edge_endpoints(edge).unwrap().1)
                    .unwrap_or(tree.node);
                assert_eq!(child.tree.node, expected_target);
                assert!(child.conditions.is_none());
                assert!(child.matching_context_ids.is_none());
                assert!(child.arguments_to_context_usages.is_none());
                prefix.push(key);
                visit(&child.tree, prefix, output);
                prefix.pop();
            }
            if tree.childs.is_empty() {
                output.insert(prefix.clone());
            }
        }

        let mut output = BTreeSet::new();
        visit(tree, &mut Vec::new(), &mut output);
        output
    }

    fn tree_from_paths(
        graph: Arc<QueryGraph>,
        root: NodeIndex,
        paths: &[OpGraphPath],
    ) -> Arc<OpPathTree> {
        let inputs = paths.iter().map(|path| (path, None)).collect::<Vec<_>>();
        Arc::new(OpPathTree::from_op_paths(graph, root, &inputs).unwrap())
    }

    fn local_selection_for_path(
        graph: &Arc<QueryGraph>,
        path: &OpGraphPath,
        alias_seed: u8,
    ) -> Arc<SelectionSet> {
        named_local_selection_for_path(graph, path, &format!("local_{alias_seed}"))
    }

    fn named_local_selection_for_path(
        graph: &Arc<QueryGraph>,
        path: &OpGraphPath,
        alias: &str,
    ) -> Arc<SelectionSet> {
        let QueryGraphNodeType::SchemaType(type_position) =
            &graph.node_weight(path.tail()).unwrap().type_
        else {
            panic!("operation path ended at a federated root")
        };
        Arc::new(
            SelectionSet::parse(
                graph.schema().unwrap().clone(),
                type_position.clone().try_into().unwrap(),
                &format!("{alias}: __typename"),
            )
            .unwrap(),
        )
    }

    fn generated_graph_path_with_composite_tail(
        graph: &Arc<QueryGraph>,
        root: NodeIndex,
        commands: &[(u16, u8)],
    ) -> OpGraphPath {
        let mut path = OpGraphPath::new(graph.clone(), root).unwrap();
        let mut tail = root;
        for &(edge_seed, trigger_variant) in commands {
            let candidates = graph
                .out_edges(tail)
                .into_iter()
                .filter(|edge| {
                    matches!(
                        &graph.node_weight(edge.target()).unwrap().type_,
                        QueryGraphNodeType::SchemaType(type_position)
                            if CompositeTypeDefinitionPosition::try_from(type_position.clone()).is_ok()
                    )
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                break;
            }
            let edge = candidates[edge_seed as usize % candidates.len()];
            let trigger = generated_trigger(graph, edge.id(), trigger_variant);
            path = path
                .add(trigger, Some(edge.id()), trivial_condition(), None)
                .unwrap();
            tail = edge.target();
        }
        path
    }

    fn tree_from_paths_with_local_selections(
        graph: Arc<QueryGraph>,
        root: NodeIndex,
        paths: &[(OpGraphPath, Arc<SelectionSet>)],
    ) -> Arc<OpPathTree> {
        let inputs = paths
            .iter()
            .map(|(path, selection)| (path, Some(selection)))
            .collect::<Vec<_>>();
        Arc::new(OpPathTree::from_op_paths(graph, root, &inputs).unwrap())
    }

    fn local_selection_model(
        paths: &[(OpGraphPath, Arc<SelectionSet>)],
    ) -> BTreeMap<SemanticBranch, BTreeSet<String>> {
        let mut model = BTreeMap::<SemanticBranch, BTreeSet<String>>::new();
        for (path, selection) in paths {
            model
                .entry(graph_path_branch(path))
                .or_default()
                .insert(selection.to_string());
        }
        model
    }

    fn audit_and_collect_local_selections(
        tree: &OpPathTree,
    ) -> BTreeMap<SemanticBranch, BTreeSet<String>> {
        fn visit(
            tree: &OpPathTree,
            prefix: &mut SemanticBranch,
            output: &mut BTreeMap<SemanticBranch, BTreeSet<String>>,
        ) {
            if !tree.local_selection_sets.is_empty() {
                let selections = tree
                    .local_selection_sets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<BTreeSet<_>>();
                output.insert(prefix.clone(), selections);
            }
            for child in &tree.childs {
                prefix.push((
                    child.edge.map(EdgeIndex::index),
                    trigger_signature(&child.trigger),
                ));
                visit(&child.tree, prefix, output);
                prefix.pop();
            }
        }

        let mut output = BTreeMap::new();
        visit(tree, &mut Vec::new(), &mut output);
        output
    }

    /// Preserve DFS order and duplicate semantic paths. Unlike the merge oracle above, this is
    /// suitable for serial mutation trees, where repeated fields must remain distinct.
    fn ordered_local_selection_occurrences(
        tree: &OpPathTree,
    ) -> Vec<(SemanticBranch, Vec<String>)> {
        fn visit(
            tree: &OpPathTree,
            prefix: &mut SemanticBranch,
            output: &mut Vec<(SemanticBranch, Vec<String>)>,
        ) {
            if !tree.local_selection_sets.is_empty() {
                output.push((
                    prefix.clone(),
                    tree.local_selection_sets
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                ));
            }
            for child in &tree.childs {
                prefix.push((
                    child.edge.map(EdgeIndex::index),
                    trigger_signature(&child.trigger),
                ));
                visit(&child.tree, prefix, output);
                prefix.pop();
            }
        }

        let mut output = Vec::new();
        visit(tree, &mut Vec::new(), &mut output);
        output
    }

    fn decorate_single_child_with_context_payload(
        tree: &Arc<OpPathTree>,
        side: &str,
        context_mask: u8,
        argument_mask: u8,
    ) -> Arc<OpPathTree> {
        let mut decorated = tree.as_ref().clone();
        assert_eq!(decorated.childs.len(), 1);
        let context_ids: IndexSet<Name> = (0..4)
            .filter(|bit| context_mask & (1 << bit) != 0)
            .map(|bit| {
                let value = format!("context_{bit}");
                Name::new(&value).unwrap()
            })
            .collect();

        let zero_path = OpGraphPath::new(tree.graph.clone(), tree.node).unwrap();
        let selection = local_selection_for_path(&tree.graph, &zero_path, argument_mask);
        let arguments: IndexMap<Name, ContextUsageEntry> = (0..4)
            .filter(|bit| argument_mask & (1 << bit) != 0)
            .map(|bit| {
                let argument = format!("{side}_argument_{bit}");
                let context_id = format!("{side}_context_{bit}");
                (
                    Name::new(&argument).unwrap(),
                    ContextUsageEntry {
                        context_id: Name::new(&context_id).unwrap(),
                        relative_path: vec![FetchDataPathElement::Parent; bit as usize],
                        selection_set: selection.as_ref().clone(),
                        subgraph_argument_type: Node::new(Type::Named(
                            Name::new("String").unwrap(),
                        )),
                    },
                )
            })
            .collect();
        let child = &decorated.childs[0];
        decorated.childs[0] = Arc::new(PathTreeChild {
            edge: child.edge,
            trigger: child.trigger.clone(),
            conditions: child.conditions.clone(),
            tree: child.tree.clone(),
            matching_context_ids: Some(context_ids),
            arguments_to_context_usages: Some(arguments),
        });
        Arc::new(decorated)
    }

    fn with_single_child_condition(
        tree: &Arc<OpPathTree>,
        condition: Arc<OpPathTree>,
    ) -> Arc<OpPathTree> {
        let mut decorated = tree.as_ref().clone();
        assert_eq!(decorated.childs.len(), 1);
        let child = &decorated.childs[0];
        decorated.childs[0] = Arc::new(PathTreeChild {
            edge: child.edge,
            trigger: child.trigger.clone(),
            conditions: Some(condition),
            tree: child.tree.clone(),
            matching_context_ids: child.matching_context_ids.clone(),
            arguments_to_context_usages: child.arguments_to_context_usages.clone(),
        });
        Arc::new(decorated)
    }

    /// Minimal fixture used only by the deterministic repros. The generated properties below use
    /// the richer interface/union fixture in `path_tree_fixture`.
    fn path_tree_repro_fixture() -> (Arc<QueryGraph>, NodeIndex) {
        let schema = Schema::parse_and_validate(
            "type Query { node: Node } type Node { id: ID!, child: Node }",
            "path-tree-repro.graphql",
        )
        .unwrap();
        let schema = ValidFederationSchema::new(schema).unwrap();
        let graph =
            Arc::new(build_query_graph("repro".into(), schema, Default::default()).unwrap());
        let root = graph.root_kinds_to_nodes().unwrap()[&SchemaRootDefinitionKind::Query];
        (graph, root)
    }

    /// Build a tree containing one real path and one trailing local selection at that path's tail.
    fn path_tree_with_local_selection(
        graph: &Arc<QueryGraph>,
        root: NodeIndex,
        path: &OpGraphPath,
        selection: &str,
    ) -> Arc<OpPathTree> {
        let tail = &graph.graph[path.tail()];
        let QueryGraphNodeType::SchemaType(type_position) = &tail.type_ else {
            panic!("local selections require a schema-type path tail")
        };
        let schema = graph.schema_by_source(&tail.source).unwrap().clone();
        let selection = Arc::new(
            SelectionSet::parse(schema, type_position.clone().try_into().unwrap(), selection)
                .unwrap(),
        );
        Arc::new(
            OpPathTree::from_op_paths(graph.clone(), root, &[(path, Some(&selection))]).unwrap(),
        )
    }

    fn one_edge_path_tree_with_condition(
        graph: &Arc<QueryGraph>,
        root: NodeIndex,
        edge: EdgeIndex,
        trigger: OpGraphPathTrigger,
        condition: Arc<OpPathTree>,
    ) -> Arc<OpPathTree> {
        let path = OpGraphPath::new(graph.clone(), root)
            .unwrap()
            .add(
                trigger,
                Some(edge),
                ConditionResolution::Satisfied {
                    cost: 1.0,
                    path_tree: Some(condition),
                    context_map: None,
                },
                None,
            )
            .unwrap();
        Arc::new(OpPathTree::from_op_paths(graph.clone(), root, &[(&path, None)]).unwrap())
    }

    fn condition_key_repro_fixture() -> (Arc<QueryGraph>, NodeIndex, EdgeIndex) {
        let supergraph = Supergraph::new_with_router_specs(include_str!(
            "../../tests/query_plan/supergraphs/can_use_a_key_on_an_interface_object_type.graphql"
        ))
        .unwrap();
        let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
        let graph = Arc::new(
            build_federated_query_graph(supergraph.schema, api_schema, None, Some(true)).unwrap(),
        );
        let key_edge = graph
            .graph
            .edge_references()
            .find(|edge| {
                if !matches!(
                    edge.weight().transition,
                    QueryGraphEdgeTransition::KeyResolution
                ) {
                    return false;
                }
                let head = &graph.graph[edge.source()];
                let tail = &graph.graph[edge.target()];
                head.source.as_ref() == "S1"
                    && tail.source.as_ref() == "S2"
                    && matches!(
                        &head.type_,
                        QueryGraphNodeType::SchemaType(
                            OutputTypeDefinitionPosition::Interface(position)
                        ) if position.type_name == "I"
                    )
            })
            .expect("fixture must contain the condition-bearing S1.I -> S2.I key")
            .id();
        let root = graph.graph.edge_endpoints(key_edge).unwrap().0;
        assert!(graph.graph[key_edge].conditions.is_some());
        (graph, root, key_edge)
    }

    fn local_selection_texts(tree: &OpPathTree) -> Vec<String> {
        tree.local_selection_sets
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    // -----------------------------------------------------------------------------------------
    // Deterministic regression repros found by the generated properties below.
    //
    // These use the same `OpGraphPath` and `OpPathTree::from_op_paths` constructors as query
    // planning. They establish loss on structurally valid planner data, but do not claim that
    // operation normalization currently emits every exact pair of trees used below. The `extend`
    // case calls out its narrower, hardening-only status separately.
    // -----------------------------------------------------------------------------------------

    /// `from_op_paths` attaches a fully-local selection at the end of a normal collected field
    /// path. Merging two trees for that same path must retain both trailing selections.
    #[test]
    fn merge_preserves_distinct_trailing_selections_on_the_same_path() {
        let (graph, root) = path_tree_repro_fixture();
        let path = build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"])
            .expect("fixture must contain Query.node");
        let left = path_tree_with_local_selection(&graph, root, &path, "left: __typename");
        let right = path_tree_with_local_selection(&graph, root, &path, "right: __typename");
        let merged = left.merge(&right);

        assert_eq!(
            merged.childs.len(),
            1,
            "the shared Query.node path must merge"
        );
        assert_eq!(
            local_selection_texts(&merged.childs[0].tree),
            vec!["{ left: __typename }", "{ right: __typename }"],
            "the shared path must retain both trailing selections",
        );
    }

    /// One closed path may end with fully-local work where another continues through a child.
    /// Merging their shared prefix must retain both the local work and the longer path.
    #[test]
    fn merge_preserves_leaf_local_selection_when_other_tree_has_children() {
        let (graph, root) = path_tree_repro_fixture();
        let leaf_path = build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"])
            .expect("fixture must contain Query.node");
        let child_path =
            build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node", "child"])
                .expect("fixture must contain Query.node.child");
        let leaf =
            path_tree_with_local_selection(&graph, root, &leaf_path, "under_node: __typename");
        let nonleaf =
            path_tree_with_local_selection(&graph, root, &child_path, "under_child: __typename");
        let merged = leaf.merge(&nonleaf);

        assert_eq!(
            merged.childs.len(),
            1,
            "the shared Query.node path must merge"
        );
        let node_tree = &merged.childs[0].tree;
        assert_eq!(
            local_selection_texts(node_tree),
            vec!["{ under_node: __typename }"],
            "merging a leaf into a branch must retain the leaf's local selection",
        );
        assert_eq!(node_tree.childs.len(), 1, "the longer path must remain");
        assert_eq!(
            local_selection_texts(&node_tree.childs[0].tree),
            vec!["{ under_child: __typename }"],
            "the longer path's trailing selection must remain at Query.node.child",
        );
    }

    /// The cheap equality predicate controls whether condition trees are merged at all. Trees
    /// carrying different local work are not equal even when both happen to be leaves.
    #[test]
    fn equals_same_root_distinguishes_different_local_selections() {
        let (graph, root) = path_tree_repro_fixture();
        let path = build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"])
            .expect("fixture must contain Query.node");
        let left = path_tree_with_local_selection(&graph, root, &path, "left: __typename");
        let right = path_tree_with_local_selection(&graph, root, &path, "right: __typename");

        assert_eq!(
            local_selection_texts(&left.childs[0].tree),
            vec!["{ left: __typename }"]
        );
        assert_eq!(
            local_selection_texts(&right.childs[0].tree),
            vec!["{ right: __typename }"]
        );
        assert!(
            !left.equals_same_root(&right),
            "trees with different trailing local selections must not compare equal",
        );
    }

    /// Hardening invariant: `extend` is currently called with mutation-root trees that have
    /// children, so this empty-child state is not known to be planner-reachable. If a valid tree
    /// carrying only root-local work is accepted, however, extending by it must not discard it.
    #[test]
    fn extend_preserves_local_selections_from_a_leaf_tree() {
        let (graph, root) = path_tree_repro_fixture();
        let leaf_path = OpGraphPath::new(graph.clone(), root).unwrap();
        let child_path = build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"])
            .expect("fixture must contain Query.node");
        let mut tree =
            path_tree_with_local_selection(&graph, root, &child_path, "under_node: __typename")
                .as_ref()
                .clone();
        let leaf = path_tree_with_local_selection(&graph, root, &leaf_path, "at_root: __typename");

        tree.extend(&leaf);
        assert_eq!(
            local_selection_texts(&tree),
            vec!["{ at_root: __typename }"],
            "extending with a leaf must retain the leaf's local selection",
        );
        assert_eq!(tree.childs.len(), 1, "the existing branch must remain");
        assert_eq!(
            local_selection_texts(&tree.childs[0].tree),
            vec!["{ under_node: __typename }"],
            "the branch's trailing selection must remain at Query.node",
        );
    }

    /// Child condition trees are merged through `equals_same_root` and `merge_if_not_equal`.
    /// Both trees satisfy the real key's required `id`; their distinct additional local work must
    /// also survive that fast path.
    #[test]
    fn merge_preserves_local_selections_from_both_child_condition_trees() {
        let (graph, root, key_edge) = condition_key_repro_fixture();
        let condition_path = OpGraphPath::new(graph.clone(), root).unwrap();
        let left_condition = path_tree_with_local_selection(
            &graph,
            root,
            &condition_path,
            "id left_condition: __typename",
        );
        let right_condition = path_tree_with_local_selection(
            &graph,
            root,
            &condition_path,
            "id right_condition: __typename",
        );
        let left = one_edge_path_tree_with_condition(
            &graph,
            root,
            key_edge,
            OpGraphPathContext::default().into(),
            left_condition,
        );
        let right = one_edge_path_tree_with_condition(
            &graph,
            root,
            key_edge,
            OpGraphPathContext::default().into(),
            right_condition,
        );

        let merged = left.merge(&right);
        let condition = merged.childs[0]
            .conditions
            .as_ref()
            .expect("shared child lost its condition tree");
        assert_eq!(
            local_selection_texts(condition),
            vec![
                "{ id left_condition: __typename }",
                "{ id right_condition: __typename }",
            ],
            "merging equal-looking condition trees must retain local selections from both",
        );
    }

    // -----------------------------------------------------------------------------------------
    // Generated coverage: semantic trie construction, union, equality, and complete payloads.
    // -----------------------------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// `from_op_paths` and `merge` are two implementations of trie union. Compare both with
        /// a flat set-of-maximal-branches model, while varying shared prefixes, aliases, casts,
        /// duplicates, input order, and merge association.
        #[test]
        fn path_tree_conserves_semantic_branches_under_build_and_merge(
            path_specs in prop::collection::vec(
                prop::collection::vec((any::<u16>(), any::<u8>()), 1..10),
                3..25,
            ),
        ) {
            let (graph, root) = path_tree_fixture();
            let paths = path_specs
                .iter()
                .map(|spec| generated_graph_path(&graph, root, spec))
                .collect::<Vec<_>>();
            let expected = maximal_path_branches(&paths);

            let tree = tree_from_paths(graph.clone(), root, &paths);
            prop_assert_eq!(audit_and_collect_tree_branches(&tree), expected.clone());

            let mut reversed = paths.clone();
            reversed.reverse();
            let reversed_tree = tree_from_paths(graph.clone(), root, &reversed);
            prop_assert_eq!(
                audit_and_collect_tree_branches(&reversed_tree),
                expected.clone()
            );

            let first_cut = paths.len() / 3;
            let second_cut = 2 * paths.len() / 3;
            let a = tree_from_paths(graph.clone(), root, &paths[..first_cut]);
            let b = tree_from_paths(graph.clone(), root, &paths[first_cut..second_cut]);
            let c = tree_from_paths(graph.clone(), root, &paths[second_cut..]);

            let left_associated = a.merge(&b).merge(&c);
            let right_associated = a.merge(&b.merge(&c));
            prop_assert_eq!(
                audit_and_collect_tree_branches(&left_associated),
                expected.clone()
            );
            prop_assert_eq!(
                audit_and_collect_tree_branches(&right_associated),
                expected.clone()
            );
            prop_assert_eq!(
                audit_and_collect_tree_branches(&b.merge(&a).merge(&c)),
                expected
            );
            prop_assert!(Arc::ptr_eq(&tree.merge(&tree), &tree));
        }

        /// Building a tree from all payload-bearing paths and unioning trees built from arbitrary
        /// partitions must place exactly the same local selections at every semantic trie node.
        #[test]
        fn path_tree_merge_matches_rebuild_for_local_selections(
            path_specs in prop::collection::vec(
                prop::collection::vec((any::<u16>(), any::<u8>()), 0..8),
                2..20,
            ),
        ) {
            let (graph, root) = path_tree_fixture();
            let paths = path_specs
                .iter()
                .enumerate()
                .map(|(index, spec)| {
                    let path = generated_graph_path_with_composite_tail(&graph, root, spec);
                    let selection = local_selection_for_path(&graph, &path, index as u8);
                    (path, selection)
                })
                .collect::<Vec<_>>();
            let expected = local_selection_model(&paths);
            let rebuilt = tree_from_paths_with_local_selections(graph.clone(), root, &paths);
            prop_assert_eq!(
                audit_and_collect_local_selections(&rebuilt),
                expected.clone(),
                "from_op_paths misplaced a trailing local selection"
            );

            let cut = paths.len() / 2;
            let left = tree_from_paths_with_local_selections(
                graph.clone(),
                root,
                &paths[..cut],
            );
            let right = tree_from_paths_with_local_selections(
                graph,
                root,
                &paths[cut..],
            );
            prop_assert_eq!(
                audit_and_collect_local_selections(&left.merge(&right)),
                expected.clone(),
                "merge misplaced or discarded a trailing local selection"
            );
            prop_assert_eq!(
                audit_and_collect_local_selections(&right.merge(&left)),
                expected,
                "merge was order-dependent for trailing local selections"
            );

            prop_assert!(
                !left.equals_same_root(&right),
                "different generated local-selection payloads compared equal"
            );
        }

        /// The production caller uses `extend` to concatenate serial mutation trees, whose paths
        /// always collect at least one root field. Under that caller-shaped precondition,
        /// extending partitions must retain the same semantic branches as the source paths.
        #[test]
        fn path_tree_extend_conserves_nonempty_mutation_shaped_paths(
            left_specs in prop::collection::vec(
                prop::collection::vec((any::<u16>(), any::<u8>()), 1..8),
                1..10,
            ),
            right_specs in prop::collection::vec(
                prop::collection::vec((any::<u16>(), any::<u8>()), 1..8),
                1..10,
            ),
        ) {
            let (graph, root) = path_tree_fixture();
            let paths = left_specs
                .iter()
                .chain(&right_specs)
                .enumerate()
                .map(|(index, spec)| {
                    let path = generated_graph_path_with_composite_tail(&graph, root, spec);
                    let selection = local_selection_for_path(&graph, &path, index as u8);
                    (path, selection)
                })
                .collect::<Vec<_>>();
            let cut = left_specs.len();
            let left = tree_from_paths_with_local_selections(graph.clone(), root, &paths[..cut]);
            let right = tree_from_paths_with_local_selections(graph, root, &paths[cut..]);
            prop_assert!(!left.childs.is_empty());
            prop_assert!(!right.childs.is_empty());

            let mut expected = ordered_local_selection_occurrences(&left);
            expected.extend(ordered_local_selection_occurrences(&right));

            let mut extended = left.as_ref().clone();
            extended.extend(&right);
            prop_assert_eq!(
                ordered_local_selection_occurrences(&extended),
                expected,
                "extend misplaced, merged, or reordered work from nonempty serial paths"
            );
        }

        /// When two paths share an edge/trigger, context metadata belongs to that semantic edge
        /// and must be unioned rather than duplicated or dropped. Argument names are disjoint
        /// across sides so the reference operation is ordinary map union with no conflict policy.
        #[test]
        fn path_tree_merge_unions_shared_child_context_payloads(
            edge_seed in any::<u16>(),
            trigger_seed in any::<u8>(),
            left_context_mask in any::<u8>(),
            right_context_mask in any::<u8>(),
            left_argument_mask in any::<u8>(),
            right_argument_mask in any::<u8>(),
            collide_argument_names in any::<bool>(),
        ) {
            let (graph, root) = path_tree_fixture();
            let path = generated_graph_path(&graph, root, &[(edge_seed, trigger_seed)]);
            let base = tree_from_paths(graph, root, &[path]);
            prop_assert_eq!(base.childs.len(), 1);
            let left = decorate_single_child_with_context_payload(
                &base,
                if collide_argument_names { "shared" } else { "left" },
                left_context_mask & 0b1111,
                left_argument_mask & 0b1111,
            );
            let right = decorate_single_child_with_context_payload(
                &base,
                if collide_argument_names { "shared" } else { "right" },
                right_context_mask & 0b1111,
                right_argument_mask & 0b1111,
            );

            let mut expected_context_ids = left.childs[0]
                .matching_context_ids
                .clone()
                .unwrap_or_default();
            expected_context_ids.extend(
                right.childs[0]
                    .matching_context_ids
                    .iter()
                    .flatten()
                    .cloned(),
            );
            let mut expected_arguments = left.childs[0]
                .arguments_to_context_usages
                .clone()
                .unwrap_or_default();
            expected_arguments.extend(
                right.childs[0]
                    .arguments_to_context_usages
                    .iter()
                    .flat_map(|arguments| arguments.iter())
                    .map(|(name, usage)| (name.clone(), usage.clone())),
            );

            let merged = left.merge(&right);
            prop_assert_eq!(merged.childs.len(), 1, "shared branch was duplicated");
            prop_assert_eq!(
                merged.childs[0]
                    .matching_context_ids
                    .as_ref()
                    .cloned()
                    .unwrap_or_default(),
                expected_context_ids,
            );
            prop_assert_eq!(
                merged.childs[0]
                    .arguments_to_context_usages
                    .as_ref()
                    .cloned()
                    .unwrap_or_default(),
                expected_arguments,
            );
        }

        /// Condition trees use the same trie-union machinery recursively. Vary the containing
        /// edge/trigger while forcing distinct trailing selections in the two condition trees.
        #[test]
        fn path_tree_merge_unions_shared_child_condition_payloads(
            edge_seed in any::<u16>(),
            trigger_seed in any::<u8>(),
            selection_seed in any::<u8>(),
            // Root-local condition payload loss already has a deterministic repro. Require a
            // nonempty condition path here so this broad property covers recursive trie nodes.
            condition_spec in prop::collection::vec((any::<u16>(), any::<u8>()), 1..7),
        ) {
            let (graph, root) = path_tree_fixture();
            let branch_path = generated_graph_path(&graph, root, &[(edge_seed, trigger_seed)]);
            let base = tree_from_paths(graph.clone(), root, &[branch_path]);
            let condition_path =
                generated_graph_path_with_composite_tail(&graph, root, &condition_spec);
            let condition_branch = graph_path_branch(&condition_path);
            let left_alias = selection_seed.wrapping_mul(2);
            let right_alias = left_alias.wrapping_add(1);
            let left_condition = tree_from_paths_with_local_selections(
                graph.clone(),
                root,
                &[(
                    condition_path.clone(),
                    local_selection_for_path(&graph, &condition_path, left_alias),
                )],
            );
            let right_condition = tree_from_paths_with_local_selections(
                graph,
                root,
                &[(
                    condition_path.clone(),
                    local_selection_for_path(&base.graph, &condition_path, right_alias),
                )],
            );

            let merged = with_single_child_condition(&base, left_condition)
                .merge(&with_single_child_condition(&base, right_condition));
            let actual = audit_and_collect_local_selections(
                merged.childs[0]
                    .conditions
                    .as_ref()
                    .expect("shared child lost its condition tree"),
            );
            let expected = BTreeMap::from([(
                condition_branch,
                BTreeSet::from([
                    format!("{{ local_{left_alias}: __typename }}"),
                    format!("{{ local_{right_alias}: __typename }}"),
                ]),
            )]);
            prop_assert_eq!(actual, expected);
        }

        /// Exercise the complete shared-edge payload at once. This catches interactions hidden by
        /// dimension-at-a-time tests: the child trie carries local work, while its incoming edge
        /// simultaneously carries a condition tree, matching context IDs, and argument usages.
        #[test]
        fn path_tree_merge_conserves_combined_recursive_edge_payload(
            edge_seed in any::<u16>(),
            trigger_seed in any::<u8>(),
            selection_seed in any::<u8>(),
            left_context_mask in 1u8..16,
            right_context_mask in 1u8..16,
            left_argument_mask in 1u8..16,
            right_argument_mask in 1u8..16,
            condition_spec in prop::collection::vec((any::<u16>(), any::<u8>()), 1..7),
        ) {
            let (graph, root) = path_tree_fixture();
            let branch_path = generated_graph_path_with_composite_tail(
                &graph,
                root,
                &[(edge_seed, trigger_seed)],
            );
            let left_alias = selection_seed.wrapping_mul(4);
            let right_alias = left_alias.wrapping_add(1);
            let left_base = tree_from_paths_with_local_selections(
                graph.clone(),
                root,
                &[(
                    branch_path.clone(),
                    local_selection_for_path(&graph, &branch_path, left_alias),
                )],
            );
            let right_base = tree_from_paths_with_local_selections(
                graph.clone(),
                root,
                &[(
                    branch_path.clone(),
                    local_selection_for_path(&graph, &branch_path, right_alias),
                )],
            );
            let condition_path =
                generated_graph_path_with_composite_tail(&graph, root, &condition_spec);
            let condition_branch = graph_path_branch(&condition_path);
            let left_condition_alias = left_alias.wrapping_add(2);
            let right_condition_alias = left_alias.wrapping_add(3);
            let left_condition = tree_from_paths_with_local_selections(
                graph.clone(),
                root,
                &[(
                    condition_path.clone(),
                    local_selection_for_path(&graph, &condition_path, left_condition_alias),
                )],
            );
            let right_condition = tree_from_paths_with_local_selections(
                graph,
                root,
                &[(
                    condition_path.clone(),
                    local_selection_for_path(&left_base.graph, &condition_path, right_condition_alias),
                )],
            );
            let left = with_single_child_condition(
                &decorate_single_child_with_context_payload(
                    &left_base,
                    "left_combined",
                    left_context_mask & 0b1111,
                    left_argument_mask & 0b1111,
                ),
                left_condition,
            );
            let right = with_single_child_condition(
                &decorate_single_child_with_context_payload(
                    &right_base,
                    "right_combined",
                    right_context_mask & 0b1111,
                    right_argument_mask & 0b1111,
                ),
                right_condition,
            );

            let mut expected_context_ids = left.childs[0]
                .matching_context_ids
                .clone()
                .unwrap_or_default();
            expected_context_ids.extend(
                right.childs[0]
                    .matching_context_ids
                    .iter()
                    .flatten()
                    .cloned(),
            );
            let mut expected_arguments = left.childs[0]
                .arguments_to_context_usages
                .clone()
                .unwrap_or_default();
            expected_arguments.extend(
                right.childs[0]
                    .arguments_to_context_usages
                    .iter()
                    .flat_map(|arguments| arguments.iter())
                    .map(|(name, usage)| (name.clone(), usage.clone())),
            );

            let merged = left.merge(&right);
            prop_assert_eq!(merged.childs.len(), 1);
            prop_assert_eq!(
                merged.childs[0]
                    .matching_context_ids
                    .clone()
                    .unwrap_or_default(),
                expected_context_ids,
            );
            prop_assert_eq!(
                merged.childs[0]
                    .arguments_to_context_usages
                    .clone()
                    .unwrap_or_default(),
                expected_arguments,
            );
            prop_assert_eq!(
                audit_and_collect_local_selections(&merged.childs[0].tree),
                BTreeMap::from([(
                    Vec::new(),
                    BTreeSet::from([
                        format!("{{ local_{left_alias}: __typename }}"),
                        format!("{{ local_{right_alias}: __typename }}"),
                    ]),
                )]),
                "shared child lost its own local payload",
            );
            prop_assert_eq!(
                audit_and_collect_local_selections(
                    merged.childs[0]
                        .conditions
                        .as_ref()
                        .expect("shared child lost its condition tree"),
                ),
                BTreeMap::from([(
                    condition_branch,
                    BTreeSet::from([
                        format!("{{ local_{left_condition_alias}: __typename }}"),
                        format!("{{ local_{right_condition_alias}: __typename }}"),
                    ]),
                )]),
                "shared child condition lost its local payload",
            );
        }
    }

    #[test]
    fn path_tree_display() {
        let src = r#"
        type Query
        {
            t: T
        }

        type T
        {
            otherId: ID!
            id: ID!
        }

        query Test
        {
            t {
                id
            }
        }
        "#;

        let (schema, mut executable_document) = parse_schema_and_operation(src);
        let (op_name, operation) = executable_document.operations.named.first_mut().unwrap();

        let query_graph = Arc::new(
            build_query_graph(
                op_name.to_string().into(),
                schema.clone(),
                Default::default(),
            )
            .unwrap(),
        );

        let path1 =
            build_graph_path(&query_graph, SchemaRootDefinitionKind::Query, &["t", "id"]).unwrap();
        assert_eq!(
            path1.to_string(),
            "Query(Test) --[t]--> T(Test) --[id]--> ID(Test)"
        );

        let path2 = build_graph_path(
            &query_graph,
            SchemaRootDefinitionKind::Query,
            &["t", "otherId"],
        )
        .unwrap();
        assert_eq!(
            path2.to_string(),
            "Query(Test) --[t]--> T(Test) --[otherId]--> ID(Test)"
        );

        let normalized_operation = normalize_operation(
            operation,
            &Default::default(),
            &schema,
            &Default::default(),
            &never_cancel,
        )
        .unwrap();
        let selection_set = Arc::new(normalized_operation.selection_set);

        let paths = vec![
            (&path1, Some(&selection_set)),
            (&path2, Some(&selection_set)),
        ];
        let path_tree = OpPathTree::from_op_paths(query_graph, NodeIndex::new(0), &paths).unwrap();
        let computed = path_tree.to_string();
        let expected = r#"Query(Test):
 -> [3] t = T(Test):
   -> [1] id = ID(Test)
   -> [0] otherId = ID(Test)"#;
        assert_eq!(computed, expected);
    }
}
