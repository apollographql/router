use std::{collections::HashMap, sync::Arc};

use crate::{
    error::SchemaRootKind,
    query_graph::{
        graph_path::{GraphPath, GraphPathTrigger, IndirectPaths},
        QueryGraph,
    },
};

use apollo_compiler::{
    ast::{Document, NamedType},
    NodeStr,
};
use itertools::Itertools;
use petgraph::graph::EdgeIndex;

type Todo = usize;

impl<TTrigger, TEdge> GraphPath<TTrigger, TEdge>
where
    TTrigger: Eq + std::hash::Hash,
    Arc<TTrigger>: Into<GraphPathTrigger>,
    TEdge: Copy + Into<Option<EdgeIndex>>,
    EdgeIndex: Into<TEdge>,
{
    pub fn from_graph_root(_graph: Arc<QueryGraph>, _root_kind: SchemaRootKind) -> Todo /* Option<Self> */
    {
        // graph
        //     .root_node_for_kind(root_kind)
        //     .map(|root| Self::new(graph, root))

        todo!()
    }
}

pub(super) fn print_subgraph_names(names: &[NodeStr]) -> String {
    print_human_readable_list(
        names.iter().map(|n| format!("\"{}\"", n)).collect(),
        None,                     // emptyValue
        Some("subgraph".into()),  // prefix
        Some("subgraphs".into()), // prefixPlural
        None,                     // lastSeparator
        None,                     // cutoff_output_length
    )
}

/// Like `joinStrings`, joins an array of string, but with a few twists, namely:
///  - If the resulting list to print is "too long", it only display a subset
///    of the elements and use some elipsis (...). In other words, this method
///    is for case where, where the list ot print is too long, it is more useful
///    to avoid flooding the output than printing everything.
///  - it allows to prefix the whole list, and to use a different prefix for a
///    single element than for > 1 elements.
///  - it forces the use of ',' as separator, but allow a different
///    lastSeparator.
pub(super) fn print_human_readable_list(
    names: Vec<String>,
    _empty_value: Option<String>,
    _prefix: Option<String>,
    _prefix_plural: Option<String>,
    _last_separator: Option<String>,
    _cutoff_ouput_length: Option<u32>,
) -> String {
    names.iter().join(", ")
}

/// PORT_NOTE: for printing "witness" operations, we actually need a printer
/// that accepts invalid selection sets.
pub(super) fn operation_to_document(_operation: Todo) -> Document {
    todo!()
}

/// Wraps a 'composition validation' path (one built from `Transition`) along
/// with the information necessary to compute the indirect paths following that
/// path, and cache the result of that computation when triggered.
///
/// In other words, this is a `GraphPath<Transition, V>` plus lazy memoization
/// of the computation of its following indirect options.
///
/// The rational is that after we've reached a given path, we might never need
/// to compute the indirect paths following it (maybe all the fields we'll care
/// about are available "directive" (from the same subgraph)), or we might need
/// to compute it once, or we might need them multiple times, but the way the
/// algorithm work, we don't know this in advance. So this abstraction ensure
/// that we only compute such indirect paths lazily, if we ever need them, but
/// while ensuring we don't recompute them multiple times if we do need them
/// multiple times.
pub(super) struct TransitionPathWithLazyIndirectPaths<TTrigger, TEdge>
where
    TTrigger: Eq + std::hash::Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
    EdgeIndex: Into<TEdge>,
    GraphPathTrigger: From<Arc<TTrigger>>,
{
    path: Todo,               // GraphPath<Transition, V>
    condition_resolver: Todo, // ConditionResolver
    override_conditions: HashMap<NodeStr, bool>,
    lazy_computed_indirect_paths: Option<IndirectPaths<TTrigger, TEdge>>, // Option<IndirectPaths<Transition, V, TEdge>>
}

impl<TTrigger, TEdge> TransitionPathWithLazyIndirectPaths<TTrigger, TEdge>
where
    TTrigger: Eq + std::hash::Hash,
    TEdge: Copy + Into<Option<EdgeIndex>>,
    EdgeIndex: Into<TEdge>,
    GraphPathTrigger: From<Arc<TTrigger>>,
{
    pub(super) fn initial(
        initial_path: Todo,       // GraphPath<Transition, V>
        condition_resolver: Todo, // ConditionResolver
        override_conditions: HashMap<NodeStr, bool>,
    ) -> Self {
        Self {
            path: initial_path,
            condition_resolver,
            override_conditions,
            lazy_computed_indirect_paths: None,
        }
    }

    pub(super) fn indirect_options(&mut self) -> IndirectPaths<TTrigger, TEdge> {
        // if let Some(indirect_paths) = self.lazy_computed_indirect_paths {
        //     return indirect_paths;
        // }
        // self.lazy_computed_indirect_paths = Some(self.compute_indirect_paths());
        // self.lazy_computed_indirect_paths.unwrap()
        self.compute_indirect_paths()
    }

    fn compute_indirect_paths(&self) -> IndirectPaths<TTrigger, TEdge> {
        // GraphPath.advance_with_non_collecting_and_type_preserving_transitions
        todo!()
    }
}

/// Note: conditions resolver should return `null` if the condition cannot be
/// satisfied. If it is satisfied, it has the choice of computing
/// the actual tree, which we need for query planning, or simply returning
/// "undefined" which means "The condition can be satisfied but I didn't
/// bother computing a tree for it", which we use for simple validation.
///
/// Returns some a `Unadvanceables` object if there is no way to advance the
/// path with this transition. Otherwise, it returns a list of options (paths)
/// we can be in after advancing the transition.
///
/// The lists of options can be empty, which has the special meaning that the
/// transition is guaranteed to have no results (it corresponds to unsatisfiable
/// conditions), meaning that as far as composition validation goes, we can
/// ignore that transition (and anything that follows) and otherwise continue.
pub(super) fn advance_path_with_transition(
    /* <V: Vertex> */
    _subgraph_path: Todo, // TransitionPathWithLazyIndirectPaths<V>,
    _transition: Todo,    // Transition,
    _target_type: NamedType,
    _override_conditions: HashMap<NodeStr, bool>,
) -> Todo /* TransitionPathWithLazyIndirectPaths<V>[] | Unadvanceables */ {
    /* !!! THIS IS A LOT !!! */
    todo!()
}
