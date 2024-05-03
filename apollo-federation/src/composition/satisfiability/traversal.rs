use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::execution::GraphQLError;
use apollo_compiler::NodeStr;
use itertools::Itertools;

use super::diagnostics::CompositionHint;
use super::state::ValidationState;
use super::ValidationContext;
use crate::error::FederationError;
use crate::query_graph::QueryGraph;
use crate::schema::ValidFederationSchema;

type Todo = usize;
static _TODO: Todo = 0;

pub(super) struct ValidationTraversal {
    condition_resolver: Todo,

    /// The stack contains all states that aren't terminal.
    stack: Vec<ValidationState>,

    /// For each vertex in the supergraph, records if we've already visited that
    /// vertex and in which subgraphs we were. For a vertex, we may have
    /// multiple "sets of subgraphs", hence the double-array.
    previous_visits: QueryGraphState<Vec<VertexVisit>>, // QueryGraphState<VertexVisit[]>

    validation_errors: Vec<GraphQLError>,
    validation_hints: Vec<CompositionHint>,

    context: ValidationContext,
}

impl ValidationTraversal {
    pub(super) fn new(
        supergraph_schema: Arc<ValidFederationSchema>,
        supergraph_api: Arc<QueryGraph>,
        federated_query_graph: Arc<QueryGraph>,
    ) -> Result<Self, FederationError> {
        let condition_resolver = _TODO; // simpleValidationConditionResolver

        let stack = supergraph_api
            .root_kinds_to_nodes()?
            .keys()
            .map(|root_kind| {
                ValidationState::initial(
                    supergraph_api.clone(),
                    *root_kind,
                    federated_query_graph.clone(),
                    condition_resolver,
                    Default::default(),
                )
            })
            .try_collect()?;

        Ok(Self {
            condition_resolver,
            stack,
            previous_visits: QueryGraphState::new(supergraph_api.clone()),
            validation_errors: vec![],
            validation_hints: vec![],
            context: ValidationContext::new(supergraph_schema),
        })
    }

    pub(super) fn validate(
        mut self,
    ) -> Result<Vec<CompositionHint>, (Vec<GraphQLError>, Vec<CompositionHint>)> {
        while let Some(state) = self.stack.pop() {
            self.handle_state(&state);
        }

        if self.validation_errors.is_empty() {
            Ok(self.validation_hints)
        } else {
            Err((self.validation_errors, self.validation_hints))
        }
    }

    fn handle_state(&mut self, _state: &ValidationState) {}
}

struct VertexVisit {
    subgraphs: Vec<NodeStr>,
    override_conditions: HashMap<NodeStr, bool>,
}

/// `maybe_superset` is a superset (or equal) if it contains all of `other`'s
/// subgraphs and all of `other`'s labels (with matching conditions).
fn is_superset_or_equal(maybe_superset: &VertexVisit, other: &VertexVisit) -> bool {
    let include_all_subgraphs = other
        .subgraphs
        .iter()
        .all(|subgraph| maybe_superset.subgraphs.contains(subgraph));

    let includes_all_override_conditions =
        other.override_conditions.iter().all(|(label, condition)| {
            maybe_superset
                .override_conditions
                .get(label)
                .map_or(false, |c| c == condition)
        });

    include_all_subgraphs && includes_all_override_conditions
}

// PORT_NOTE: For satisfiability, this is just a map of node indexes to node[].
// Leaving off the Edge state for now.
struct QueryGraphState<VertexState> {
    vertices_states: Vec<Option<VertexState>>,
}

impl<VertexState> QueryGraphState<VertexState> {
    fn new(graph: Arc<QueryGraph>) -> Self {
        Self {
            vertices_states: Vec::with_capacity(graph.graph().node_count()),
        }
    }

    fn set_vertex_state(&mut self, _vertex: usize /* Vertext */, _state: VertexState) {
        todo!()
    }

    fn get_vertex_state(&self, _vertex: usize /* Vertex */) -> Option<&VertexState> {
        todo!()
    }

    // unneeded?
    // fn remove_vertex_state
    // fn set_edge_state
    // fn remove_edge_state
    // fn get_edge_state
}
