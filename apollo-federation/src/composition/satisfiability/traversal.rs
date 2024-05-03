use std::{collections::HashMap, sync::Arc};

use apollo_compiler::{execution::GraphQLError, NodeStr};

use crate::{query_graph::QueryGraph, schema::ValidFederationSchema};

use super::{diagnostics::CompositionHint, state::ValidationState, ValidationContext};

type Todo = usize;
static _TODO: Todo = 0;

pub(super) struct ValidationTraversal {
    condition_resolver: Todo,

    /// The stack contains all states that aren't terminal.
    stack: Vec<ValidationState>,

    /// For each vertex in the supergraph, records if we've already visited that
    /// vertex and in which subgraphs we were. For a vertex, we may have
    /// multiple "sets of subgraphs", hence the double-array.
    previous_visits: Todo, // QueryGraphState<VertexVisit[]>

    validation_errors: Vec<GraphQLError>,
    validation_hints: Vec<CompositionHint>,

    context: ValidationContext,
}

impl ValidationTraversal {
    pub(super) fn new(
        supergraph_schema: Arc<ValidFederationSchema>, // Schema
        _supergraph_api: Arc<QueryGraph>,
        _federated_query_graph: Arc<QueryGraph>,
    ) -> Self {
        let condition_resolver = _TODO; // simpleValidationConditionResolver

        let stack = vec![];

        Self {
            condition_resolver,
            stack,
            previous_visits: _TODO, // QueryGraphState::new(_supergraph_api),
            validation_errors: vec![],
            validation_hints: vec![],
            context: ValidationContext::new(supergraph_schema),
        }
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

struct QueryGraphState<VertexState, EdgeState> {
    vertices_states: Vec<Option<VertexState>>,
    adjancencies_states: Vec<Vec<Option<EdgeState>>>,
}

impl<VertexState, EdgeState> QueryGraphState<VertexState, EdgeState> {
    fn new(_graph: QueryGraph) -> Self {
        todo!()
    }

    fn set_vertex_state(&mut self, _vertex: usize /* Vertext */, _state: VertexState) {
        todo!()
    }

    // fn remove_vertex_state
    // fn get_vertex_state
    // fn set_edge_state
    // fn remove_edge_state
    // fn get_edge_state
}
