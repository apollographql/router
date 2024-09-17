use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use indexmap::map::Entry;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use serde::Serialize;

use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::query_graph::graph_path::GraphPathItem;
use crate::query_graph::graph_path::OpGraphPath;
use crate::query_graph::graph_path::OpGraphPathTrigger;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphNode;
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
            if include_conditions {
                if let Some(ref child_cond) = child.conditions {
                    write!(f, "!! {{\n{indent} ")?;
                    child_cond.fmt_internal(f, &child_indent, /*include_conditions*/ true)?;
                    write!(f, "\n{indent} }}")?;
                }
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

impl<TTrigger, TEdge> PathTree<TTrigger, TEdge>
where
    TTrigger: Eq + Hash,
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
            by_unique_trigger:
                IndexMap<&'inputs Arc<TTrigger>, PathTreeChildInputs<'inputs, GraphPathIter>>,
        }

        struct PathTreeChildInputs<'inputs, GraphPathIter> {
            conditions: Option<Arc<OpPathTree>>,
            sub_paths_and_selections: Vec<(GraphPathIter, Option<&'inputs Arc<SelectionSet>>)>,
        }

        let mut local_selection_sets = Vec::new();

        for (mut graph_path_iter, selection) in graph_paths_and_selections {
            let Some((generic_edge, trigger, conditions)) = graph_path_iter.next() else {
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
                    existing.conditions = merge_conditions(&existing.conditions, conditions);
                    existing
                        .sub_paths_and_selections
                        .push((graph_path_iter, selection))
                    // Note that as we merge, we don't create a new child
                }
                Entry::Vacant(entry) => {
                    entry.insert(PathTreeChildInputs {
                        conditions: conditions.clone(),
                        sub_paths_and_selections: vec![(graph_path_iter, selection)],
                    });
                }
            }
        }

        let mut childs = Vec::new();
        for (edge, by_unique_edge) in merged {
            for (trigger, child) in by_unique_edge.by_unique_trigger {
                childs.push(Arc::new(PathTreeChild {
                    edge,
                    trigger: trigger.clone(),
                    conditions: child.conditions.clone(),
                    tree: Arc::new(Self::from_paths(
                        graph.clone(),
                        by_unique_edge.target_node,
                        child.sub_paths_and_selections,
                    )?),
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
            || self.childs.iter().zip(&other.childs).all(|(a, b)| {
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
        if self == other {
            return;
        }
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
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use petgraph::stable_graph::NodeIndex;
    use petgraph::visit::EdgeRef;

    use crate::error::FederationError;
    use crate::operation::normalize_operation;
    use crate::operation::Field;
    use crate::operation::FieldData;
    use crate::query_graph::build_query_graph::build_query_graph;
    use crate::query_graph::condition_resolver::ConditionResolution;
    use crate::query_graph::graph_path::OpGraphPath;
    use crate::query_graph::graph_path::OpGraphPathTrigger;
    use crate::query_graph::graph_path::OpPathElement;
    use crate::query_graph::path_tree::OpPathTree;
    use crate::query_graph::QueryGraph;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::schema::position::SchemaRootDefinitionKind;
    use crate::schema::ValidFederationSchema;

    // NB: stole from operation.rs
    fn parse_schema_and_operation(
        schema_and_operation: &str,
    ) -> (ValidFederationSchema, ExecutableDocument) {
        let (schema, executable_document) =
            apollo_compiler::parse_mixed_validate(schema_and_operation, "document.graphql")
                .unwrap();
        let executable_document = executable_document.into_inner();
        let schema = ValidFederationSchema::new(schema).unwrap();
        (schema, executable_document)
    }

    fn trivial_condition() -> ConditionResolution {
        ConditionResolution::Satisfied {
            cost: 0.0,
            path_tree: None,
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
            let data = FieldData {
                schema: query_graph.schema().unwrap().clone(),
                field_position: field_def.clone(),
                alias: None,
                arguments: Default::default(),
                directives: Default::default(),
                sibling_typename: None,
            };
            let trigger = OpGraphPathTrigger::OpPathElement(OpPathElement::Field(Field::new(data)));

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

        let query_graph =
            Arc::new(build_query_graph(op_name.to_string().into(), schema.clone(), None).unwrap());

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

        let normalized_operation =
            normalize_operation(operation, Default::default(), &schema, &Default::default())
                .unwrap();
        let selection_set = Arc::new(normalized_operation.selection_set);

        let paths = vec![
            (&path1, Some(&selection_set)),
            (&path2, Some(&selection_set)),
        ];
        let path_tree =
            OpPathTree::from_op_paths(query_graph.to_owned(), NodeIndex::new(0), &paths).unwrap();
        let computed = path_tree.to_string();
        let expected = r#"Query(Test):
 -> [3] t = T(Test):
   -> [1] id = ID(Test)
   -> [0] otherId = ID(Test)"#;
        assert_eq!(computed, expected);
    }
}
