use std::borrow::Cow;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use tracing::debug;
use tracing::debug_span;

use crate::composition::satisfiability::validation_context::ValidationContext;
use crate::composition::satisfiability::validation_state::SubgraphContextKey;
use crate::composition::satisfiability::validation_state::ValidationState;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::merger::merge::CompositionOptions;
use crate::operation::SelectionSet;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::condition_resolver::CachingConditionResolver;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolverCache;
use crate::query_graph::graph_path::ExcludedConditions;
use crate::query_graph::graph_path::ExcludedDestinations;
use crate::query_graph::graph_path::operation::OpGraphPathContext;
use crate::schema::ValidFederationSchema;
use crate::schema::position::FieldDefinitionPosition;
use crate::supergraph::CompositionHint;

pub(super) struct ValidationTraversal {
    top_level_condition_resolver: TopLevelConditionResolver,
    /// The stack of non-terminal states left to traverse.
    stack: Vec<ValidationState>,
    /// The previous visits for a node in the API schema query graph.
    previous_visits: IndexMap<NodeIndex, Vec<NodeVisit>>,
    validation_errors: Vec<CompositionError>,
    validation_hints: Vec<CompositionHint>,
    /// When we discover a shared top-level mutation field, we track satisfiability errors for
    /// each subgraph containing the field separately. This is because the query planner needs to
    /// avoid calling these fields more than once, which means there must be no satisfiability
    /// errors for (at least) one subgraph. The first key is the field coordinate, and the second
    /// key is the subgraph name.
    satisfiability_errors_by_mutation_field_and_subgraph:
        IndexMap<FieldDefinitionPosition, IndexMap<Arc<str>, Vec<CompositionError>>>,
    context: ValidationContext,
    total_validation_subgraph_paths: usize,
    max_validation_subgraph_paths: usize,
}

struct TopLevelConditionResolver {
    /// The federated query graph for the supergraph schema.
    query_graph: Arc<QueryGraph>,
    /// The cache for top-level condition resolution.
    condition_resolver_cache: ConditionResolverCache,
}

/// When we visit a node in the API schema query graph, we keep track of any information about the
/// simultaneous subgraph paths that may affect what options are available downstream. This is
/// currently:
/// 1. For each subgraph path, the subgraph of the path's tail along with the types and subgraphs of
///    any context matches encountered along that path.
/// 2. Any progressive override labels that we've assumed the value of while taking the API schema
///    query graph path.
///
/// If we ever re-visit the node with at least more options than a prior visit while making at least
/// as many assumptions, then we know we can skip re-visiting.
struct NodeVisit {
    subgraph_context_keys: IndexSet<SubgraphContextKey>,
    override_conditions: Arc<OverrideConditions>,
}

impl NodeVisit {
    /// Determines if this visit is a non-strict superset of the `other` visit, meaning that this
    /// visit has at least as many options as the `other` visit while making at least as many
    /// assumptions.
    // PORT_NOTE: Named `isSupersetOrEqual()` in the JS codebase, but supersets are by default
    // non-strict and Rust typically names such methods as `is_superset()`.
    fn is_superset(&self, other: &NodeVisit) -> bool {
        self.subgraph_context_keys
            .is_superset(&other.subgraph_context_keys)
            && other
                .override_conditions
                .iter()
                .all(|(label, is_enabled)| self.override_conditions.get(label) == Some(is_enabled))
    }
}

impl ValidationTraversal {
    const DEFAULT_MAX_VALIDATION_SUBGRAPH_PATHS: usize = 1_000_000;

    pub(super) fn new(
        supergraph_schema: ValidFederationSchema,
        api_schema_query_graph: Arc<QueryGraph>,
        federated_query_graph: Arc<QueryGraph>,
        composition_options: &CompositionOptions,
    ) -> Result<Self, FederationError> {
        let mut validation_traversal = Self {
            top_level_condition_resolver: TopLevelConditionResolver {
                query_graph: federated_query_graph.clone(),
                condition_resolver_cache: ConditionResolverCache::new(),
            },
            stack: vec![],
            previous_visits: Default::default(),
            validation_errors: vec![],
            validation_hints: vec![],
            satisfiability_errors_by_mutation_field_and_subgraph: Default::default(),
            context: ValidationContext::new(supergraph_schema)?,
            total_validation_subgraph_paths: 0,
            max_validation_subgraph_paths: composition_options
                .max_validation_subgraph_paths
                .unwrap_or(Self::DEFAULT_MAX_VALIDATION_SUBGRAPH_PATHS),
        };
        for kind in api_schema_query_graph.root_kinds_to_nodes()?.keys() {
            validation_traversal.push_stack(ValidationState::new(
                api_schema_query_graph.clone(),
                federated_query_graph.clone(),
                *kind,
            )?);
        }
        Ok(validation_traversal)
    }

    fn push_stack(&mut self, state: ValidationState) -> Option<CompositionError> {
        self.total_validation_subgraph_paths += state.subgraph_path_infos().len();
        self.stack.push(state);
        if self.total_validation_subgraph_paths > self.max_validation_subgraph_paths {
            Some(CompositionError::MaxValidationSubgraphPathsExceeded {
                message: format!(
                    "Maximum number of validation subgraph paths exceeded: {}",
                    self.total_validation_subgraph_paths
                ),
            })
        } else {
            None
        }
    }

    fn pop_stack(&mut self) -> Option<ValidationState> {
        if let Some(state) = self.stack.pop() {
            self.total_validation_subgraph_paths -= state.subgraph_path_infos().len();
            Some(state)
        } else {
            None
        }
    }

    pub(super) fn validate(
        &mut self,
        errors: &mut Vec<CompositionError>,
        hints: &mut Vec<CompositionHint>,
    ) -> Result<(), FederationError> {
        while let Some(state) = self.pop_stack() {
            if let Some(error) = self.handle_state(state)? {
                // Certain errors during satisfiability can cause the algorithm to abort to avoid
                // resource exhaustion; when this occurs, we only report that specific error.
                errors.push(error);
                hints.append(&mut self.validation_hints);
                return Ok(());
            }
        }

        // Check if any shared top-level mutation fields have errors in all subgraphs
        for (field_coordinate, errors_by_subgraph) in
            &self.satisfiability_errors_by_mutation_field_and_subgraph
        {
            // Check if some subgraph has no satisfiability errors. If so, then that subgraph
            // can be used to satisfy all queries to the top-level mutation field, and we can
            // ignore the errors in other subgraphs.
            let some_subgraph_has_no_errors = errors_by_subgraph.values().any(|e| e.is_empty());
            if some_subgraph_has_no_errors {
                continue;
            }

            // Otherwise, queries on the top-level mutation field can't be satisfied through
            // only one call to that field.
            let mut message_parts = vec![format!(
                "Supergraph API queries using the mutation field \"{}\" at top-level must be \
                satisfiable without needing to call that field from multiple subgraphs, but \
                every subgraph with that field encounters satisfiability errors. Please fix \
                these satisfiability errors for (at least) one of the following subgraphs with \
                the mutation field:",
                field_coordinate
            )];

            for (subgraph, subgraph_errors) in errors_by_subgraph {
                message_parts.push(format!(
                    "- When calling \"{}\" at top-level from subgraph \"{}\":",
                    field_coordinate, subgraph
                ));
                for error in subgraph_errors {
                    for line in error.to_string().lines() {
                        if line.is_empty() {
                            message_parts.push(String::new());
                        } else {
                            message_parts.push(format!("  {}", line));
                        }
                    }
                }
            }

            self.validation_errors
                .push(CompositionError::SatisfiabilityError {
                    message: message_parts.join("\n"),
                });
        }

        errors.append(&mut self.validation_errors);
        hints.append(&mut self.validation_hints);
        Ok(())
    }

    fn handle_state(
        &mut self,
        mut state: ValidationState,
    ) -> Result<Option<CompositionError>, FederationError> {
        debug!(
            "Validation: {} open states. Validating {}",
            self.stack.len() + 1,
            state,
        );
        let span = debug_span!(" |");
        let guard = span.enter();
        let current_node = state.supergraph_path().tail();
        let current_visit = NodeVisit {
            subgraph_context_keys: state.current_subgraph_context_keys()?,
            override_conditions: state.selected_override_conditions().clone(),
        };
        let previous_visits_for_node = self.previous_visits.entry(current_node).or_default();
        for previous_visit in previous_visits_for_node.iter() {
            if current_visit.is_superset(previous_visit) {
                // This means that we've already seen the type and subgraph we're currently on in
                // the supergraph, and for that previous visit, we've either finished validating we
                // could reach anything from there, or are in the middle of it (in which case, we're
                // in a loop). Since we have at least as many options while making at least as many
                // assumptions as the previous visit, we can handle downstream operation elements
                // the same way we did previously, and so we skip the node entirely.
                drop(guard);
                debug!("Have already validated this node.");
                return Ok(None);
            }
        }
        // We have to validate this node, but we can save the visit here to potentially avoid later
        // visits.
        previous_visits_for_node.push(current_visit);

        // Pre-collect the next edges for `state.supergraph_path()`, since as we iterate through
        // these edges, we're going to be mutating the cache in `state.subgraph_paths()`.
        //
        // Note that if the `supergraph_path()` is terminal, this method is a no-op, which is
        // expected/desired as it means we've successfully "validated" a path to its end.
        let edges = state.supergraph_path().next_edges()?.collect::<Vec<_>>();
        for edge in edges {
            let edge_weight = state.supergraph_path().graph().edge_weight(edge)?;
            let mut edge_head_type_name = None;
            if let QueryGraphEdgeTransition::FieldCollection {
                field_definition_position,
                ..
            } = &edge_weight.transition
            {
                if field_definition_position.is_introspection_typename_field() {
                    // There is no point in validating __typename edges, since we know we can always
                    // get those.
                    continue;
                } else {
                    // If this edge is a field, then later we'll need the field's parent type.
                    edge_head_type_name = Some(field_definition_position.type_name());
                }
            }

            // `selected_override_conditions()` indicates the labels (and their respective
            // conditions) that we've selected/assumed so far in our traversal (i.e. "foo" -> true).
            // There's no need to validate edges that share the same label with the opposite
            // condition since they're unreachable during query planning.
            if let Some(override_condition) = &edge_weight.override_condition
                && state
                    .selected_override_conditions()
                    .contains_key(&override_condition.label)
                && !override_condition.check(state.selected_override_conditions())
            {
                debug!(
                    "Edge {} doesn't satisfy label condition: {}({}), no need to validate further",
                    edge_weight,
                    override_condition.label,
                    state
                        .selected_override_conditions()
                        .get(&override_condition.label)
                        .map_or("unset".to_owned(), |x| x.to_string()),
                );
                continue;
            }

            let matching_contexts = edge_head_type_name
                .and_then(|name| self.context.matching_contexts(name))
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(IndexSet::default()));

            debug!("Validating supergraph edge {}", edge_weight);
            let span = debug_span!(" |");
            let guard = span.enter();
            let num_errors = self.validation_errors.len();
            let new_state = state.validate_transition(
                &self.context,
                edge,
                matching_contexts.as_ref(),
                &mut self.top_level_condition_resolver,
                &mut self.validation_errors,
                &mut self.validation_hints,
                &mut self.satisfiability_errors_by_mutation_field_and_subgraph,
            )?;
            if num_errors != self.validation_errors.len() {
                drop(guard);
                debug!("Validation error!");
                continue;
            }

            // The check for `is_terminal()` is not strictly necessary, since if we add a terminal
            // state to the stack, then `handle_state()` will do nothing later. But it's worth
            // checking it now and saving some memory/cycles.
            if let Some(new_state) = new_state
                && !new_state
                    .supergraph_path()
                    .graph()
                    .is_terminal(new_state.supergraph_path().tail())
            {
                drop(guard);
                debug!("Reached new state {}", new_state);
                if let Some(error) = self.push_stack(new_state) {
                    return Ok(Some(error));
                }
                continue;
            }
            drop(guard);
            debug!("Reached terminal node/cycle")
        }

        Ok(None)
    }
}

impl CachingConditionResolver for TopLevelConditionResolver {
    fn query_graph(&self) -> &QueryGraph {
        &self.query_graph
    }

    fn resolver_cache(&mut self) -> &mut ConditionResolverCache {
        &mut self.condition_resolver_cache
    }

    fn resolve_without_cache(
        &self,
        edge: EdgeIndex,
        context: &OpGraphPathContext,
        excluded_destinations: &ExcludedDestinations,
        excluded_conditions: &ExcludedConditions,
        extra_conditions: Option<&SelectionSet>,
    ) -> Result<ConditionResolution, FederationError> {
        crate::composition::satisfiability::conditions_validation::resolve_condition_plan(
            self.query_graph.clone(),
            edge,
            context,
            excluded_destinations,
            excluded_conditions,
            extra_conditions,
        )
    }
}
