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
    fn equals_same_root(self: &Arc<Self>, other: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, other)
            || self.local_selection_sets == other.local_selection_sets
                && self.childs.len() == other.childs.len()
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

    /// Appends the other's children and local selections without merging or reordering them.
    /// In particular, repeated children must remain separate for serial mutation execution.
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
        // A leaf can still carry fully-local work; only a payload-free leaf is an identity.
        if other.childs.is_empty() && other.local_selection_sets.is_empty() {
            return self.clone();
        }
        if self.childs.is_empty() && self.local_selection_sets.is_empty() {
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
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::parser::Parser;
    use petgraph::graph::EdgeIndex;
    use petgraph::stable_graph::NodeIndex;
    use petgraph::visit::EdgeRef;

    use crate::Supergraph;
    use crate::error::FederationError;
    use crate::operation::Field;
    use crate::operation::Selection;
    use crate::operation::SelectionSet;
    use crate::operation::never_cancel;
    use crate::operation::normalize_operation;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::query_graph::QueryGraphNodeType;
    use crate::query_graph::build_federated_query_graph;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::query_graph::condition_resolver::ConditionResolution;
    use crate::query_graph::graph_path::operation::OpGraphPath;
    use crate::query_graph::graph_path::operation::OpGraphPathContext;
    use crate::query_graph::graph_path::operation::OpGraphPathTrigger;
    use crate::query_graph::graph_path::operation::OpPathElement;
    use crate::query_graph::path_tree::OpPathTree;
    use crate::schema::ValidFederationSchema;
    use crate::schema::position::OutputTypeDefinitionPosition;
    use crate::schema::position::SchemaRootDefinitionKind;

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

    fn flat_local_fields(selection: &SelectionSet) -> BTreeSet<String> {
        selection
            .selections
            .values()
            .map(|selection| {
                let Selection::Field(field) = selection else {
                    panic!("fixture expects scalar fields")
                };
                assert!(
                    field.selection_set.is_none(),
                    "local-payload oracle expects scalar fields"
                );
                selection.to_string()
            })
            .collect()
    }

    fn local_selection_texts(tree: &OpPathTree) -> Vec<String> {
        tree.local_selection_sets
            .iter()
            .flat_map(|selection| flat_local_fields(selection))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

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
            vec!["left: __typename", "right: __typename"],
            "the shared path must retain both trailing selections",
        );
    }

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
        for merged in [leaf.merge(&nonleaf), nonleaf.merge(&leaf)] {
            assert_eq!(
                merged.childs.len(),
                1,
                "the shared Query.node path must merge"
            );
            let node_tree = &merged.childs[0].tree;
            assert_eq!(
                local_selection_texts(node_tree),
                vec!["under_node: __typename"],
                "merging a leaf into a branch must retain the leaf's local selection",
            );
            assert_eq!(node_tree.childs.len(), 1, "the longer path must remain");
            assert_eq!(
                local_selection_texts(&node_tree.childs[0].tree),
                vec!["under_child: __typename"],
                "the longer path's trailing selection must remain at Query.node.child",
            );
        }
    }

    #[test]
    fn equals_same_root_distinguishes_different_local_selections() {
        let (graph, root) = path_tree_repro_fixture();
        let path = build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"])
            .expect("fixture must contain Query.node");
        let left = path_tree_with_local_selection(&graph, root, &path, "left: __typename");
        let right = path_tree_with_local_selection(&graph, root, &path, "right: __typename");

        assert_eq!(
            local_selection_texts(&left.childs[0].tree),
            vec!["left: __typename"]
        );
        assert_eq!(
            local_selection_texts(&right.childs[0].tree),
            vec!["right: __typename"]
        );
        assert!(
            !left.equals_same_root(&right),
            "trees with different trailing local selections must not compare equal",
        );
    }

    #[test]
    fn extend_preserves_local_selections_and_child_order() {
        let (graph, root) = path_tree_repro_fixture();
        let root_path = OpGraphPath::new(graph.clone(), root).unwrap();
        let child_path =
            build_graph_path(&graph, SchemaRootDefinitionKind::Query, &["node"]).unwrap();
        let leaf = path_tree_with_local_selection(&graph, root, &root_path, "at_root: __typename");
        let branch =
            path_tree_with_local_selection(&graph, root, &child_path, "under_node: __typename");

        for (left, right) in [(&leaf, &branch), (&branch, &leaf)] {
            let mut extended = left.as_ref().clone();
            extended.extend(right);
            assert_eq!(
                local_selection_texts(&extended),
                vec!["at_root: __typename"]
            );
            assert_eq!(extended.childs.len(), 1);
            assert_eq!(
                local_selection_texts(&extended.childs[0].tree),
                vec!["under_node: __typename"]
            );
        }
        let second =
            path_tree_with_local_selection(&graph, root, &child_path, "second: __typename");
        let mut serial = branch.as_ref().clone();
        serial.extend(&second);
        serial.extend(&branch);
        assert_eq!(serial.childs.len(), 3);
        for (child, expected) in serial.childs.iter().zip([
            "under_node: __typename",
            "second: __typename",
            "under_node: __typename",
        ]) {
            assert_eq!(local_selection_texts(&child.tree), vec![expected]);
        }
        assert!(Arc::ptr_eq(&branch.merge(&branch), &branch));
        let mut repeated = branch.as_ref().clone();
        repeated.extend(&branch);
        assert_eq!(repeated.childs.len(), 2);
        assert!(!branch.equals_same_root(&Arc::new(repeated)));
        let empty = Arc::new(OpPathTree::new(graph, root));
        assert!(!empty.equals_same_root(&branch));
        assert!(!branch.equals_same_root(&empty));
    }

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
                "id",
                "left_condition: __typename",
                "right_condition: __typename",
            ],
            "merging equal-looking condition trees must retain local selections from both",
        );
    }

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
