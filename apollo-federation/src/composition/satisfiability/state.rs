use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Display, Formatter},
    sync::Arc,
};

use apollo_compiler::{execution::GraphQLError, NodeStr};
use itertools::Itertools;

use crate::{error::SchemaRootKind, query_graph::QueryGraph};

use super::{diagnostics::CompositionHint, ValidationContext};

type Todo = usize;
static _TODO: Todo = 0;

pub(super) struct ValidationState {
    /// Path in the supergraph corresponding to the current state.
    pub(super) supergraph_path: Todo, // RootPath<Transition>

    /// All the possible paths we could be in the subgraph.
    pub(super) subgraph_paths: Vec<Todo>, // TransitionPathWithLazyIndirectPaths<RootVertex>[]

    /// When we encounter an `@override`n field with a label condition, we record
    /// its value (T/F) as we traverse the graph. This allows us to ignore paths
    /// that can never be taken by the query planner (i.e. a path where the
    /// condition is T in one case and F in another).
    pub(super) selected_override_conditions: HashMap<NodeStr, bool>,
}

impl ValidationState {
    pub(super) fn initial(
        _supergraph_api: Arc<QueryGraph>,
        kind: SchemaRootKind,
        federated_query_graph: Arc<QueryGraph>,
        _condition_resolver: Todo, // ConditionResolver
        _override_conditions: HashMap<NodeStr, bool>,
    ) -> Result<Self, Todo> {
        Ok(Self {
            supergraph_path: _TODO, // GraphPath::from_graph_root(_supergraph_api, _kind),
            subgraph_paths: initial_subgraph_paths(kind, federated_query_graph)?, // .map(p => TransitionPathWithLazyIndirectPaths.initial(p, _condition_resolver, _override_conditions)),
            selected_override_conditions: Default::default(),
        })
    }

    /// Validates that the current state can always be advanced for the provided
    /// supergraph edge, and returns the updated state if so.
    ///
    /// # Arguments
    ///
    /// * supergraphEdge - the edge to try to advance from the current state.
    ///
    /// # Returns
    ///
    /// An object with `error` set if the state _cannot_ be properly advanced
    /// (and if so, `state` and `hint` will be `undefined`). If the state can be
    /// successfully advanced, then `state` contains the updated new state. This
    /// *can* be `undefined` to signal that the state _can_ be successfully
    /// advanced (no error) but is guaranteed to yield no results (in other
    /// words, the edge corresponds to a type condition for which there cannot
    /// be any runtime types), in which case not further validation is necessary
    /// "from that branch". Additionally, when the state can be successfully
    /// advanced, an `hint` can be optionally returned.
    pub(super) fn validate_transition(
        &self,
        _context: &ValidationContext,
        _supergraph_edge: Todo, // Edge
    ) -> Result<(Self, Option<CompositionHint>), GraphQLError> {
        // advance_path_with_transition
        // satisfiability_error
        // possible_runtime_type_names_sorted
        // shareable_field_non_intersecting_runtime_types_error
        // shareable_field_mismatched_runtime_types_hint

        let new_override_conditions = self.selected_override_conditions.clone();
        let new_subgraph_paths = vec![];
        let new_path = _TODO; // self.supergraph_path.add()

        let updated_state = Self {
            supergraph_path: new_path,
            subgraph_paths: new_subgraph_paths,
            selected_override_conditions: new_override_conditions,
        };

        Ok((updated_state, None))
    }

    pub(super) fn current_subgraph_names(&self) -> HashSet<NodeStr> {
        todo!()
    }

    pub(super) fn current_subgraphs(&self) -> Vec<Todo> /* (name: NodeStr, subgraph: Subgraph)[] */
    {
        todo!()
    }
}

impl Display for ValidationState {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let supergraph_path = self.supergraph_path;
        let subgraph_paths = self.subgraph_paths.iter().map(|p| p.to_string()).join(", ");
        write!(f, "{supergraph_path} <=> [{subgraph_paths}]")
    }
}

fn initial_subgraph_paths(
    _kind: SchemaRootKind,
    _subgraphs: Arc<QueryGraph>,
) -> Result<Vec<Todo>, Todo> /* RootPath<Transition>[], can error */ {
    todo!()
}

fn possible_runtime_type_names_sorted(_path: Todo /* RootPath<Transition> */) -> Vec<String> {
    todo!()
}
