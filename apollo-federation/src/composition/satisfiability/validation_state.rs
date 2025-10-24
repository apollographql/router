use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use either::Either;
use itertools::Itertools;
use petgraph::graph::EdgeIndex;
use petgraph::visit::EdgeRef;

use crate::bail;
use crate::composition::satisfiability::satisfiability_error::satisfiability_error;
use crate::composition::satisfiability::satisfiability_error::shareable_field_mismatched_runtime_types_hint;
use crate::composition::satisfiability::satisfiability_error::shareable_field_non_intersecting_runtime_types_error;
use crate::composition::satisfiability::validation_context::ValidationContext;
use crate::ensure;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolver;
use crate::query_graph::graph_path::GraphPathWeightCounter;
use crate::query_graph::graph_path::UnadvanceableClosures;
use crate::query_graph::graph_path::Unadvanceables;
use crate::query_graph::graph_path::transition::TransitionGraphPath;
use crate::query_graph::graph_path::transition::TransitionPathWithLazyIndirectPaths;
use crate::schema::position::AbstractTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::supergraph::CompositionHint;
use crate::utils::iter_into_single_item;

pub(super) struct ValidationState {
    /// Path in the supergraph (i.e. the API schema query graph) corresponding to the current state.
    supergraph_path: TransitionGraphPath,
    /// All the possible paths we could be in the subgraphs (excluding @provides paths).
    subgraph_paths: Vec<SubgraphPathInfo>,
    /// When we encounter a supergraph field with a progressive override (i.e. an @override with a
    /// label condition), we consider both possibilities for the label value (T/F) as we traverse
    /// the graph, and record that here. This allows us to exclude paths that can never be taken by
    /// the query planner (i.e. a path where the condition is T in one case and F in another).
    selected_override_conditions: Arc<OverrideConditions>,
}

pub(super) struct SubgraphPathInfo {
    path: TransitionPathWithLazyIndirectPaths,
    contexts: SubgraphPathContexts,
}

/// A map from context names to information about their match in the subgraph path, if it exists.
/// This is a `BTreeMap` to support `Hash`, as this is used in keys in maps.
type SubgraphPathContexts = Arc<BTreeMap<String, SubgraphPathContextInfo>>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct SubgraphPathContextInfo {
    subgraph_name: Arc<str>,
    type_name: Name,
}

#[derive(PartialEq, Eq, Hash)]
pub(super) struct SubgraphContextKey {
    tail_subgraph_name: Arc<str>,
    contexts: SubgraphPathContexts,
}

impl ValidationState {
    pub(super) fn supergraph_path(&self) -> &TransitionGraphPath {
        &self.supergraph_path
    }

    pub(super) fn subgraph_paths(&self) -> &Vec<SubgraphPathInfo> {
        &self.subgraph_paths
    }

    pub(super) fn selected_override_conditions(&self) -> &Arc<OverrideConditions> {
        &self.selected_override_conditions
    }

    // PORT_NOTE: Named `initial()` in the JS codebase, but conventionally in Rust this kind of
    // constructor is named `new()`.
    pub(super) fn new(
        api_schema_query_graph: Arc<QueryGraph>,
        federated_query_graph: Arc<QueryGraph>,
        root_kind: SchemaRootDefinitionKind,
        graph_path_weight_counter: Arc<GraphPathWeightCounter>,
    ) -> Result<Self, FederationError> {
        let Some(federated_root_node) =
            federated_query_graph.root_kinds_to_nodes()?.get(&root_kind)
        else {
            bail!(
                "The supergraph shouldn't have a {} root if no subgraphs have one",
                root_kind
            );
        };
        let federated_root_node_weight = federated_query_graph.node_weight(*federated_root_node)?;
        ensure!(
            federated_root_node_weight.type_ == QueryGraphNodeType::FederatedRootType(root_kind),
            "Unexpected node type {} for federated query graph root (expected {})",
            federated_root_node_weight.type_,
            QueryGraphNodeType::FederatedRootType(root_kind),
        );
        let initial_subgraph_path = TransitionGraphPath::from_graph_root(
            federated_query_graph.clone(),
            root_kind,
            graph_path_weight_counter.clone(),
        )?;
        Ok(Self {
            supergraph_path: TransitionGraphPath::from_graph_root(
                api_schema_query_graph,
                root_kind,
                graph_path_weight_counter,
            )?,
            subgraph_paths: federated_query_graph
                .out_edges(*federated_root_node)
                .into_iter()
                .map(|edge_ref| {
                    let path = initial_subgraph_path.add(
                        QueryGraphEdgeTransition::SubgraphEnteringTransition,
                        edge_ref.id(),
                        ConditionResolution::no_conditions(),
                        None,
                    )?;
                    Ok::<_, FederationError>(SubgraphPathInfo {
                        path: TransitionPathWithLazyIndirectPaths::new(Arc::new(path)),
                        contexts: Default::default(),
                    })
                })
                .process_results(|iter| iter.collect())?,
            selected_override_conditions: Default::default(),
        })
    }

    /// Validates that the current state can always be advanced for the provided supergraph edge,
    /// and returns the updated state if so.
    ///
    /// If the state cannot be properly advanced, then an error will be pushed onto the provided
    /// `errors` array, nothing will be pushed onto the provided `hints` array, and no updated state
    /// will be returned. Otherwise, nothing will be pushed onto the `errors` array, a hint may be
    /// pushed onto the `hints` array, and an updated state will be returned except for the case
    /// where the transition is guaranteed to yield no results (in which case no state is returned).
    /// This exception occurs when the edge corresponds to a type condition that does not intersect
    /// with the possible runtime types of the old path's tail, in which case further validation on
    /// the new path is not necessary.
    pub(super) fn validate_transition(
        &mut self,
        context: &ValidationContext,
        supergraph_edge: EdgeIndex,
        matching_contexts: &IndexSet<String>,
        condition_resolver: &mut impl ConditionResolver,
        errors: &mut Vec<CompositionError>,
        hints: &mut Vec<CompositionHint>,
    ) -> Result<Option<ValidationState>, FederationError> {
        let edge_weight = self.supergraph_path.graph().edge_weight(supergraph_edge)?;
        ensure!(
            edge_weight.conditions.is_none(),
            "Supergraph edges should not have conditions ({})",
            edge_weight,
        );
        let (_, transition_tail) = self
            .supergraph_path
            .graph()
            .edge_endpoints(supergraph_edge)?;
        let transition_tail_weight = self.supergraph_path.graph().node_weight(transition_tail)?;
        let QueryGraphNodeType::SchemaType(target_type) = &transition_tail_weight.type_ else {
            bail!("Unexpectedly encountered federation root node as tail node.");
        };
        let new_override_conditions =
            if let Some(override_condition) = &edge_weight.override_condition {
                let mut conditions = self.selected_override_conditions.as_ref().clone();
                conditions.insert(
                    override_condition.label.clone(),
                    override_condition.condition,
                );
                Arc::new(conditions)
            } else {
                self.selected_override_conditions.clone()
            };

        let mut new_subgraph_paths: Vec<SubgraphPathInfo> = Default::default();
        let mut dead_ends: Vec<UnadvanceableClosures> = Default::default();
        for SubgraphPathInfo { path, contexts } in self.subgraph_paths.iter_mut() {
            let options = path.advance_with_transition(
                &edge_weight.transition,
                target_type,
                self.supergraph_path.graph().schema()?,
                condition_resolver,
                &new_override_conditions,
            )?;
            let options = match options {
                Either::Left(options) => options,
                Either::Right(closures) => {
                    dead_ends.push(closures);
                    continue;
                }
            };
            if options.is_empty() {
                // This means that the edge is a type condition and that if we follow the path in
                // this subgraph, we're guaranteed that handling that type condition give us no
                // matching results, and so we can skip this supergraph path entirely.
                return Ok(None);
            }
            let new_contexts = if matching_contexts.is_empty() {
                contexts.clone()
            } else {
                let tail_weight = path.path.graph().node_weight(path.path.tail())?;
                let tail_subgraph = tail_weight.source.clone();
                let QueryGraphNodeType::SchemaType(tail_type) = &tail_weight.type_ else {
                    bail!("Unexpectedly encountered federation root node as tail node.");
                };
                let mut contexts = contexts.as_ref().clone();
                for matching_context in matching_contexts {
                    contexts.insert(
                        matching_context.clone(),
                        SubgraphPathContextInfo {
                            subgraph_name: tail_subgraph.clone(),
                            type_name: tail_type.type_name().clone(),
                        },
                    );
                }
                Arc::new(contexts)
            };
            new_subgraph_paths.extend(options.into_iter().map(|option| SubgraphPathInfo {
                path: option,
                contexts: new_contexts.clone(),
            }))
        }
        let new_supergraph_path = self.supergraph_path.add(
            edge_weight.transition.clone(),
            supergraph_edge,
            ConditionResolution::no_conditions(),
            None,
        )?;
        if new_subgraph_paths.is_empty() {
            satisfiability_error(
                &new_supergraph_path,
                &self
                    .subgraph_paths
                    .iter()
                    .map(|path| path.path.path.as_ref())
                    .collect::<Vec<_>>(),
                &dead_ends
                    .into_iter()
                    .map(Unadvanceables::try_from)
                    .process_results(|iter| iter.collect::<Vec<_>>())?,
                errors,
            )?;
            return Ok(None);
        }

        let updated_state = ValidationState {
            supergraph_path: new_supergraph_path,
            subgraph_paths: new_subgraph_paths,
            selected_override_conditions: new_override_conditions,
        };

        // When handling a @shareable field, we also compare the set of runtime types for each of
        // the subgraphs involved. If there is no common intersection between those sets, then we
        // record an error: a @shareable field should resolve the same way in all the subgraphs in
        // which it is resolved, and there is no way this can be true if each subgraph returns
        // runtime objects that we know can never be the same.
        //
        // Additionally, if those sets of runtime types are not the same, we let it compose, but we
        // log a warning. Indeed, having different runtime types is a red flag: it would be
        // incorrect for a subgraph to resolve to an object of a type that the other subgraph cannot
        // possibly return, so having some subgraph having types that the other doesn't know feels
        // like something worth double-checking on the user side. Of course, as long as there is
        // some runtime types intersection and the field resolvers only return objects of that
        // intersection, then this could be a valid implementation. And this case can in particular
        // happen temporarily as subgraphs evolve (potentially independently), but it is well worth
        // warning in general.

        // Note that we ignore any path where the type is not an abstract type, because in practice
        // this means an @interfaceObject and this should not be considered as an implementation
        // type. Besides, @interfaceObject types always "stand-in" for every implementation type so
        // they're never a problem for this check and can be ignored.
        if updated_state.subgraph_paths.len() < 2 {
            return Ok(Some(updated_state));
        }
        let QueryGraphEdgeTransition::FieldCollection {
            field_definition_position,
            ..
        } = &edge_weight.transition
        else {
            return Ok(Some(updated_state));
        };
        let new_supergraph_path_tail_weight = updated_state
            .supergraph_path
            .graph()
            .node_weight(updated_state.supergraph_path.tail())?;
        let QueryGraphNodeType::SchemaType(new_supergraph_path_tail_type) =
            &new_supergraph_path_tail_weight.type_
        else {
            bail!("Unexpectedly encountered federation root node as tail node.");
        };
        if AbstractTypeDefinitionPosition::try_from(new_supergraph_path_tail_type.clone()).is_err()
        {
            return Ok(Some(updated_state));
        }
        if !context.is_shareable(field_definition_position)? {
            return Ok(Some(updated_state));
        }
        let filtered_paths_count = updated_state
            .subgraph_paths
            .iter()
            .map(|path| {
                let path_tail_weight = path.path.path.graph().node_weight(path.path.path.tail())?;
                let QueryGraphNodeType::SchemaType(path_tail_type) = &path_tail_weight.type_ else {
                    bail!("Unexpectedly encountered federation root node as tail node.");
                };
                Ok::<_, FederationError>(path_tail_type)
            })
            .process_results(|iter| {
                iter.filter(|type_pos| {
                    AbstractTypeDefinitionPosition::try_from((*type_pos).clone()).is_ok()
                })
                .count()
            })?;
        if filtered_paths_count < 2 {
            return Ok(Some(updated_state));
        }

        // We start our intersection by using all the supergraph path types, both because it's a
        // convenient "max" set to start our intersection, but also because that means we will
        // ignore @inaccessible types in our checks (which is probably not very important because
        // I believe the rules of @inaccessible kind of exclude having them here, but if that ever
        // changes, it makes more sense this way).
        let all_runtime_types = BTreeSet::from_iter(
            updated_state
                .supergraph_path()
                .runtime_types_of_tail()
                .iter()
                .map(|type_pos| type_pos.type_name.clone()),
        );
        let mut intersection = all_runtime_types.clone();

        let mut runtime_types_to_subgraphs: IndexMap<Arc<BTreeSet<Name>>, IndexSet<Arc<str>>> =
            Default::default();
        let mut runtime_types_per_subgraphs: IndexMap<Arc<str>, Arc<BTreeSet<Name>>> =
            Default::default();
        let mut has_all_empty = true;
        for new_subgraph_path in updated_state.subgraph_paths.iter() {
            let new_subgraph_path_tail_weight = new_subgraph_path
                .path
                .path
                .graph()
                .node_weight(new_subgraph_path.path.path.tail())?;
            let subgraph = &new_subgraph_path_tail_weight.source;
            let type_names = Arc::new(BTreeSet::from_iter(
                new_subgraph_path
                    .path
                    .path
                    .runtime_types_of_tail()
                    .iter()
                    .map(|type_pos| type_pos.type_name.clone()),
            ));

            // If we see a type here that is not included in the list of all runtime types, it is
            // safe to assume that it is an interface behaving like a runtime type (i.e. an
            // @interfaceObject) and we should allow it to stand in for any runtime type.
            if let Some(type_name) = iter_into_single_item(type_names.iter())
                && !all_runtime_types.contains(type_name)
            {
                continue;
            }
            runtime_types_per_subgraphs.insert(subgraph.clone(), type_names.clone());
            // PORT_NOTE: The JS code couldn't really use sets as map keys, so it instead used the
            // formatted output text as the map key. We instead use a `BTreeSet<Name>`, and move
            // the formatting logic into `shareable_field_non_intersecting_runtime_types_error()`.
            runtime_types_to_subgraphs
                .entry(type_names.clone())
                .or_default()
                .insert(subgraph.clone());
            if !type_names.is_empty() {
                has_all_empty = false;
            }
            intersection.retain(|type_name| type_names.contains(type_name));
        }

        // If `has_all_empty` is true, then it means that none of the subgraphs define any runtime
        // types. If this occurs, typically all subgraphs define a given interface, but none have
        // implementations. In that case, the intersection will be empty, but it's actually fine
        // (which is why we special case this). In fact, assuming valid GraphQL subgraph servers
        // (and it's not the place to sniff for non-compliant subgraph servers), the only value to
        // which each subgraph can resolve is `null` and so that essentially guarantees that all
        // subgraphs do resolve the same way.
        if !has_all_empty {
            if intersection.is_empty() {
                shareable_field_non_intersecting_runtime_types_error(
                    &updated_state,
                    field_definition_position,
                    &runtime_types_to_subgraphs,
                    errors,
                )?;
                return Ok(None);
            }

            // As we said earlier, we accept it if there's an intersection, but if the runtime types
            // are not all the same, we still emit a warning to make it clear that the fields should
            // not resolve any of the types not in the intersection.
            if runtime_types_to_subgraphs.len() > 1 {
                shareable_field_mismatched_runtime_types_hint(
                    &updated_state,
                    field_definition_position,
                    &intersection,
                    &runtime_types_per_subgraphs,
                    hints,
                )?;
            }
        }

        Ok(Some(updated_state))
    }

    pub(super) fn current_subgraph_names(&self) -> Result<IndexSet<Arc<str>>, FederationError> {
        self.subgraph_paths
            .iter()
            .map(|path_info| {
                Ok(path_info
                    .path
                    .path
                    .graph()
                    .node_weight(path_info.path.path.tail())?
                    .source
                    .clone())
            })
            .process_results(|iter| iter.collect())
    }

    pub(super) fn current_subgraph_context_keys(
        &self,
    ) -> Result<IndexSet<SubgraphContextKey>, FederationError> {
        self.subgraph_paths
            .iter()
            .map(|path_info| {
                Ok(SubgraphContextKey {
                    tail_subgraph_name: path_info
                        .path
                        .path
                        .graph()
                        .node_weight(path_info.path.path.tail())?
                        .source
                        .clone(),
                    contexts: path_info.contexts.clone(),
                })
            })
            .process_results(|iter| iter.collect())
    }
}

impl Display for ValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.supergraph_path.fmt(f)?;
        write!(f, " <=> ")?;
        let mut iter = self.subgraph_paths.iter();
        if let Some(first_path_info) = iter.next() {
            first_path_info.path.fmt(f)?;
            for path_info in iter {
                write!(f, ", ")?;
                path_info.path.fmt(f)?;
            }
        }
        Ok(())
    }
}
