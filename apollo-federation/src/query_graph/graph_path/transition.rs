use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Not;
use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use either::Either;
use itertools::Itertools;
use petgraph::graph::EdgeIndex;
use tracing::debug;
use tracing::debug_span;

use crate::bail;
use crate::display_helpers::DisplaySlice;
use crate::ensure;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::Selection;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolver;
use crate::query_graph::condition_resolver::UnsatisfiedConditionReason;
use crate::query_graph::graph_path::GraphPath;
use crate::query_graph::graph_path::GraphPathTriggerVariant;
use crate::query_graph::graph_path::IndirectPaths;
use crate::query_graph::graph_path::Unadvanceable;
use crate::query_graph::graph_path::UnadvanceableClosure;
use crate::query_graph::graph_path::UnadvanceableClosures;
use crate::query_graph::graph_path::UnadvanceableReason;
use crate::query_graph::graph_path::Unadvanceables;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::OutputTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::utils::human_readable::HumanReadableListOptions;
use crate::utils::human_readable::HumanReadableListPrefix;
use crate::utils::human_readable::human_readable_list;
use crate::utils::human_readable::human_readable_subgraph_names;
use crate::utils::iter_into_single_item;

/// A `GraphPath` whose triggers are query graph transitions in some other query graph (essentially
/// meaning that the path has been guided by a walk through that other query graph).
pub(crate) type TransitionGraphPath = GraphPath<QueryGraphEdgeTransition, EdgeIndex>;

impl GraphPathTriggerVariant for QueryGraphEdgeTransition {
    fn get_field_parent_type(&self) -> Option<CompositeTypeDefinitionPosition> {
        match self {
            QueryGraphEdgeTransition::FieldCollection {
                field_definition_position,
                ..
            } => Some(field_definition_position.parent()),
            _ => None,
        }
    }

    fn get_field_mut(&mut self) -> Option<&mut Field> {
        None
    }

    fn get_op_path_element(&self) -> Option<&super::operation::OpPathElement> {
        None
    }
}

/// Wraps a "composition validation" path (one built from `QueryGraphEdgeTransition`s) along with
/// some of the information necessary to compute the indirect paths following that path, and caches
/// the result of that computation when triggered.
///
/// In other words, this is a `GraphPath<QueryGraphEdgeTransition, EdgeIndex>` plus lazy memoization
/// of the computation of its following indirect options.
///
/// The rationale is that after we've reached a given path, we might never need to compute the
/// indirect paths following it (maybe all the fields we'll care about will be available "directly",
/// i.e. from the same subgraph), or we might need to compute it once, or we might need those paths
/// multiple times; but the way the algorithm works, we don't know this in advance. So this
/// abstraction ensures that we only compute such indirect paths lazily, if we ever need them, while
/// ensuring we don't recompute those paths multiple times if we do need them multiple times.
#[derive(Clone)]
pub(crate) struct TransitionPathWithLazyIndirectPaths {
    pub(crate) path: Arc<TransitionGraphPath>,
    pub(crate) lazily_computed_indirect_paths: Option<TransitionIndirectPaths>,
}

impl Display for TransitionPathWithLazyIndirectPaths {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

pub(crate) type TransitionIndirectPaths =
    IndirectPaths<QueryGraphEdgeTransition, EdgeIndex, UnadvanceableClosures>;

impl Clone for TransitionIndirectPaths {
    fn clone(&self) -> Self {
        Self {
            paths: self.paths.clone(),
            dead_ends: self.dead_ends.clone(),
        }
    }
}

impl TransitionGraphPath {
    /// Because Fed 1 used to (somewhat wrongly) require @external on key fields of type extensions
    /// and because Fed 2 allows you to avoid type extensions, users upgrading might try to remove
    /// `extend` from their schema, but forget to remove the @external on their key field. The
    /// problem is that doing that makes the key field truly external, and that could easily make
    /// @requires condition not satisfiable (because the key you'd need to get for @requires is now
    /// external). To help the user understand that mistake, we add a specific mention to this
    /// potential problem if the type is indeed an entity.
    fn warn_on_key_fields_marked_external(
        subgraph_schema: &ValidFederationSchema,
        type_: &CompositeTypeDefinitionPosition,
    ) -> Result<String, FederationError> {
        let Some(metadata) = subgraph_schema.subgraph_metadata() else {
            bail!("Type should originate from a federation subgraph schema");
        };
        let key_directive_definition_name = &metadata
            .federation_spec_definition()
            .key_directive_definition(subgraph_schema)?
            .name;
        let type_def = type_.get(subgraph_schema.schema())?;
        if !type_def.directives().has(key_directive_definition_name) {
            return Ok("".to_owned());
        }
        let external_directive_definition_name = &metadata
            .federation_spec_definition()
            .external_directive_definition(subgraph_schema)?
            .name;
        let key_fields_marked_external = type_def
            .directives()
            .get_all(key_directive_definition_name)
            .map(|application| {
                let arguments = metadata
                    .federation_spec_definition()
                    .key_directive_arguments(application)?;
                let fields = parse_field_set(
                    subgraph_schema,
                    type_def.name().clone(),
                    arguments.fields,
                    true,
                )?;
                Ok::<_, FederationError>(fields.into_iter().flat_map(|field| {
                    let Selection::Field(field) = field else {
                        return None;
                    };
                    if !field
                        .field
                        .directives
                        .has(external_directive_definition_name)
                    {
                        return None;
                    }
                    Some(field.field.name().clone())
                }))
            })
            .process_results(|iter| iter.flatten().collect::<IndexSet<_>>())?;
        if key_fields_marked_external.is_empty() {
            return Ok("".to_owned());
        }
        // PORT_NOTE: The JS codebase did the same logic here as `human_readable_list()`, except it:
        // 1. didn't impose a max output length, and
        // 2. didn't insert an "and" between the last two elements, if multiple.
        //
        // These are oversights, and we accordingly just call `human_readable_list()` here, and
        // impose the default output length limit of 100 characters.
        let fields_list = human_readable_list(
            key_fields_marked_external
                .iter()
                .map(|field| format!("\"{field}\"")),
            HumanReadableListOptions {
                prefix: Some(HumanReadableListPrefix {
                    singular: "field",
                    plural: "fields",
                }),
                ..Default::default()
            },
        );
        Ok(format!(
            " (please ensure that this is not due to key {fields_list} being accidentally marked @external)"
        ))
    }

    // PORT_NOTE: Named `findOverriddingSourcesIfOverridden()` in the JS codebase, but we've changed
    // sources to subgraphs here to clarify that we don't need to handle the federated root source.
    fn find_overriding_subgraphs_if_overridden(
        field_pos_in_subgraph: &FieldDefinitionPosition,
        field_subgraph: &Arc<str>,
        subgraphs: &IndexMap<Arc<str>, ValidFederationSchema>,
    ) -> Result<Vec<Arc<str>>, FederationError> {
        subgraphs
            .iter()
            .map(|(other_subgraph, other_subgraph_schema)| {
                if other_subgraph == field_subgraph {
                    return Ok(None);
                }
                let Some(type_pos_in_other_subgraph) = other_subgraph_schema
                    .try_get_type(field_pos_in_subgraph.parent().type_name().clone())
                else {
                    return Ok(None);
                };
                let TypeDefinitionPosition::Object(type_pos_in_other_subgraph) =
                    type_pos_in_other_subgraph
                else {
                    return Ok(None);
                };
                let Ok(field_in_other_subgraph) = type_pos_in_other_subgraph
                    .field(field_pos_in_subgraph.field_name().clone())
                    .get(other_subgraph_schema.schema())
                else {
                    return Ok(None);
                };
                let Some(metadata) = other_subgraph_schema.subgraph_metadata() else {
                    bail!("Subgraph schema unexpectedly missing metadata");
                };
                // TODO: The @override directive may genuinely not exist in the subgraph schema, and
                // override_directive_definition() should arguably return a `Result<Option<_>, _>`
                // instead, to distinguish between that case and invariant violations.
                let Ok(override_directive_definition) = metadata
                    .federation_spec_definition()
                    .override_directive_definition(other_subgraph_schema)
                else {
                    return Ok(None);
                };
                let Some(application) = field_in_other_subgraph
                    .directives
                    .get(&override_directive_definition.name)
                else {
                    return Ok(None);
                };
                if field_subgraph.as_ref()
                    != metadata
                        .federation_spec_definition()
                        .override_directive_arguments(application)?
                        .from
                {
                    return Ok(None);
                }
                Ok(Some(other_subgraph.clone()))
            })
            .process_results(|iter| iter.flatten().collect())
    }

    /// For the first element of the pair, the data has the same meaning as in
    /// `SimultaneousPathsWithLazyIndirectPaths.advance_with_operation_element()`. We also actually
    /// need to return a `Vec` of options of simultaneous paths (because when we type explode, we
    /// create simultaneous paths, but as a field might be resolved by multiple subgraphs, we may
    /// have also created multiple options).
    ///
    /// For the second element, it is true if the result only has type-exploded results.
    // PORT_NOTE: In the JS codebase, this was named `advancePathWithDirectTransition`.
    fn advance_with_transition(
        &self,
        transition: &QueryGraphEdgeTransition,
        condition_resolver: &mut impl ConditionResolver,
        override_conditions: &Arc<OverrideConditions>,
    ) -> Result<Either<Vec<Arc<TransitionGraphPath>>, UnadvanceableClosures>, FederationError> {
        ensure!(
            transition.collect_operation_elements(),
            "Supergraphs shouldn't have transitions that don't collect elements",
        );
        let mut to_advance = Either::Left(self);
        if let QueryGraphEdgeTransition::FieldCollection {
            source,
            field_definition_position,
            ..
        } = transition
        {
            let tail_weight = self.graph.node_weight(self.tail)?;
            if let Ok(tail_type) =
                CompositeTypeDefinitionPosition::try_from(tail_weight.type_.clone())
                && field_definition_position.parent().type_name() != tail_type.type_name()
                && !self.tail_is_interface_object()?
            {
                // Usually, when we collect a field, the path should already be on the type of
                // that field. But one exception is due to the fact that a type condition may be
                // "absorbed" by an @interfaceObject, and once we've taken a key on the
                // interface to another subgraph (the tail is not the interface object anymore),
                // we need to "restore" the type condition first.
                let updated_path = self.advance_with_transition(
                    &QueryGraphEdgeTransition::Downcast {
                        source: source.clone(),
                        from_type_position: tail_type.clone(),
                        to_type_position: field_definition_position.parent().clone(),
                    },
                    condition_resolver,
                    override_conditions,
                )?;
                // The case we described above should be the only case we capture here, and so
                // the current subgraph must have the implementation type (it may not have the
                // field we want, but it must have the type) and so we should be able to advance
                // to it.
                let Either::Left(updated_path) = updated_path else {
                    bail!(
                        "Advancing {} for {} unexpectedly gave unadvanceables",
                        self,
                        transition,
                    );
                };
                // Also note that there is currently no case where we should have more than one
                // option.
                let num_options = updated_path.len();
                let Some(updated_path) = iter_into_single_item(updated_path.into_iter()) else {
                    bail!(
                        "Advancing {} for {} unexpectedly gave {} options",
                        self,
                        transition,
                        num_options,
                    );
                };
                to_advance = Either::Right(updated_path);
                // We can now continue on dealing with the actual field.
            }
        }

        let mut options: Vec<Arc<TransitionGraphPath>> = vec![];
        let mut dead_end_closures: Vec<UnadvanceableClosure> = vec![];

        // Due to output type covariance, a downcast supergraph transition may be a no-op on the
        // subgraph path. In these cases, we effectively ignore the type condition.
        if let QueryGraphEdgeTransition::Downcast {
            to_type_position, ..
        } = transition
        {
            let casted_type = to_type_position.type_name();
            let tail = to_advance.graph.node_weight(to_advance.tail)?;
            if let QueryGraphNodeType::SchemaType(tail_type) = &tail.type_
                && tail_type.type_name() == casted_type
            {
                let path = match to_advance {
                    Either::Left(path) => Arc::new(path.clone()),
                    Either::Right(path) => path,
                };
                return Ok(Either::Left(vec![path]));
            }
        }

        for edge in to_advance.next_edges()? {
            let edge_weight = to_advance.graph.edge_weight(edge)?;
            // The edge must match the transition. If it doesn't, we cannot use it.
            if !edge_weight
                .transition
                .matches_supergraph_transition(transition)?
            {
                continue;
            }

            if edge_weight.override_condition.is_some()
                && !edge_weight.satisfies_override_conditions(override_conditions)
            {
                let graph = to_advance.graph.clone();
                let override_conditions = override_conditions.clone();
                dead_end_closures.push(Arc::new(LazyLock::new(Box::new(move || {
                    let edge_weight = graph.edge_weight(edge)?;
                    let Some(override_condition) = &edge_weight.override_condition else {
                        bail!("Edge unexpectedly had no override condition");
                    };
                    let (head, tail) = graph.edge_endpoints(edge)?;
                    let head_weight = graph.node_weight(head)?;
                    let tail_weight = graph.node_weight(tail)?;
                    Ok(Unadvanceables(Arc::new(vec![Arc::new(Unadvanceable {
                        reason: UnadvanceableReason::UnsatisfiableOverrideCondition,
                        from_subgraph: head_weight.source.clone(),
                        to_subgraph: tail_weight.source.clone(),
                        details: format!(
                            "Unable to take edge {} because override condition \"{}\" is {}",
                            edge_weight,
                            override_condition.label,
                            override_conditions.contains_key(&override_condition.label)
                        ),
                    })])))
                }))));
                continue;
            }

            // Additionally, we can only take an edge if we can satisfy its conditions.
            let condition_resolution = self.can_satisfy_conditions(
                edge,
                condition_resolver,
                &Default::default(),
                &Default::default(),
                &Default::default(),
            )?;
            match condition_resolution {
                ConditionResolution::Satisfied { .. } => {
                    options.push(Arc::new(to_advance.add(
                        transition.clone(),
                        edge,
                        condition_resolution,
                        None,
                    )?));
                }
                ConditionResolution::Unsatisfied { reason } => {
                    let transition = transition.clone();
                    let graph = to_advance.graph.clone();
                    dead_end_closures.push(Arc::new(LazyLock::new(Box::new(move || {
                        let edge_weight = graph.edge_weight(edge)?;
                        let (head, _) = graph.edge_endpoints(edge)?;
                        let head_weight = graph.node_weight(head)?;
                        match &edge_weight.transition {
                            QueryGraphEdgeTransition::FieldCollection {
                                field_definition_position,
                                ..
                            } => {
                                // Condition on a field means a @requires.
                                let subgraph_schema =
                                    graph.schema_by_source(&head_weight.source)?;
                                let parent_type_pos_in_subgraph: CompositeTypeDefinitionPosition =
                                    subgraph_schema.get_type(
                                        field_definition_position.parent().type_name().clone()
                                    )?.try_into()?;
                                let details = match reason {
                                    Some(UnsatisfiedConditionReason::NoPostRequireKey) => {
                                        // PORT_NOTE: The original JS codebase was printing
                                        // "@require" in the error message, this has been fixed
                                        // below to "@requires".
                                        format!(
                                            "@requires condition on field \"{}\" can be satisfied but missing usable key on \"{}\" in subgraph \"{}\" to resume query",
                                            field_definition_position,
                                            parent_type_pos_in_subgraph,
                                            head_weight.source,
                                        )
                                    }
                                    Some(UnsatisfiedConditionReason::NoSetContext) => {
                                        format!(
                                            "could not find a match for required context for field \"{field_definition_position}\"",
                                        )
                                    }
                                    None => {
                                        // TODO: This isn't necessarily just because an @requires
                                        // condition was unsatisfied, but could also be because a
                                        // @fromContext condition was unsatisfied.
                                        // PORT_NOTE: The original JS codebase was printing
                                        // "@require" in the error message, this has been fixed
                                        // below to "@requires".
                                        format!(
                                            "cannot satisfy @requires conditions on field \"{}\"{}",
                                            field_definition_position,
                                            Self::warn_on_key_fields_marked_external(
                                                subgraph_schema,
                                                &parent_type_pos_in_subgraph,
                                            )?,
                                        )
                                    }
                                };
                                Ok(Unadvanceables(Arc::new(vec![Arc::new(Unadvanceable {
                                    reason: UnadvanceableReason::UnsatisfiableRequiresCondition,
                                    from_subgraph: head_weight.source.clone(),
                                    to_subgraph: head_weight.source.clone(),
                                    details,
                                })])))
                            }
                            QueryGraphEdgeTransition::InterfaceObjectFakeDownCast {
                                from_type_position,
                                ..
                            } => {
                                // The condition on such edges is only __typename, so it essentially
                                // means that an @interfaceObject exists but there is no reachable
                                // subgraph with a @key on an interface to find out the proper
                                // implementation.
                                let details = match reason {
                                    Some(UnsatisfiedConditionReason::NoPostRequireKey) => {
                                        format!(
                                            "@interfaceObject type \"{}\" misses a resolvable key to resume query once the implementation type has been resolved",
                                            from_type_position.type_name(),
                                        )
                                    }
                                    _ => {
                                        format!(
                                            "no subgraph can be reached to resolve the implementation type of @interfaceObject type \"{}\"",
                                            from_type_position.type_name(),
                                        )
                                    }
                                };
                                Ok(Unadvanceables(Arc::new(vec![Arc::new(Unadvanceable {
                                    reason: UnadvanceableReason::UnresolvableInterfaceObject,
                                    from_subgraph: head_weight.source.clone(),
                                    to_subgraph: head_weight.source.clone(),
                                    details,
                                })])))
                            }
                            _ => {
                                bail!(
                                "Shouldn't have conditions on direct transition {}",
                                transition,
                            );
                            }
                        }
                    }))));
                }
            }
        }

        if !options.is_empty() {
            return Ok(Either::Left(options));
        }

        let transition = transition.clone();
        let graph = self.graph.clone();
        let tail = self.tail;
        Ok(Either::Right(UnadvanceableClosures(Arc::new(vec![
            Arc::new(LazyLock::new(Box::new(move || {
                let dead_ends: Unadvanceables =
                    UnadvanceableClosures(Arc::new(dead_end_closures)).try_into()?;
                if !dead_ends.is_empty() {
                    return Ok(dead_ends);
                }
                let tail_weight = graph.node_weight(tail)?;
                let QueryGraphNodeType::SchemaType(tail_type_pos) = &tail_weight.type_ else {
                    bail!("Subgraph path tail was unexpectedly a federation root node");
                };
                let subgraph = &tail_weight.source;
                let details = match transition {
                    QueryGraphEdgeTransition::FieldCollection {
                        field_definition_position,
                        ..
                    } => 'details: {
                        let subgraph_schema = graph.schema_by_source(subgraph)?;
                        let Some(metadata) = subgraph_schema.subgraph_metadata() else {
                            bail!("Type should originate from a federation subgraph schema");
                        };
                        let external_directive_definition_name = &metadata
                            .federation_spec_definition()
                            .external_directive_definition(subgraph_schema)?
                            .name;
                        let parent_type_pos = field_definition_position.parent();
                        let parent_type_pos_in_subgraph =
                            subgraph_schema.try_get_type(parent_type_pos.type_name().clone());
                        if parent_type_pos_in_subgraph.is_none()
                            && tail_type_pos.type_name() != parent_type_pos.type_name()
                        {
                            // This is due to us looking for an implementation field, but the
                            // subgraph not having that implementation because it uses
                            // @interfaceObject on an interface of that implementation.
                            break 'details format!(
                                "cannot find implementation type \"{parent_type_pos}\" (supergraph interface \"{tail_type_pos}\" is declared with @interfaceObject in \"{subgraph}\")",
                            );
                        }
                        let Some(Ok(parent_type_pos_in_subgraph)) = parent_type_pos_in_subgraph
                            .map(CompositeTypeDefinitionPosition::try_from)
                        else {
                            break 'details format!(
                                "cannot find field \"{field_definition_position}\"",
                            );
                        };
                        let Ok(field_pos_in_subgraph) = parent_type_pos_in_subgraph
                            .field(field_definition_position.field_name().clone())
                        else {
                            break 'details format!(
                                "cannot find field \"{field_definition_position}\"",
                            );
                        };
                        let Ok(field_in_subgraph) =
                            field_pos_in_subgraph.get(subgraph_schema.schema())
                        else {
                            break 'details format!(
                                "cannot find field \"{field_definition_position}\"",
                            );
                        };
                        // The subgraph has the field but no corresponding edge. This should only
                        // happen if the field is external.
                        let Some(application) = field_in_subgraph
                            .directives
                            .get(external_directive_definition_name)
                        else {
                            bail!(
                                "{} in {} is not external but there is no corresponding edge",
                                field_pos_in_subgraph,
                                subgraph,
                            );
                        };
                        // The field is external in the subgraph extracted from the supergraph, but
                        // it might have been forced to be external due to being a "used" overridden
                        // field, in which case we want to amend the message to avoid confusing the
                        // user. Note that the subgraph extraction marks such "forced external due
                        // to being overridden" fields by setting the "reason" to "[overridden]".
                        let external_reason = metadata
                            .federation_spec_definition()
                            .external_directive_arguments(application)?
                            .reason;
                        let overriding_subgraphs = if external_reason == Some("[overridden]") {
                            Self::find_overriding_subgraphs_if_overridden(
                                &field_pos_in_subgraph,
                                subgraph,
                                graph.subgraph_schemas(),
                            )?
                        } else {
                            vec![]
                        };
                        if overriding_subgraphs.is_empty() {
                            format!(
                                "field \"{field_definition_position}\" is not resolvable because marked @external",
                            )
                        } else {
                            format!(
                                "field \"{}\" is not resolvable because it is overridden by {}",
                                field_definition_position,
                                human_readable_subgraph_names(overriding_subgraphs.iter()),
                            )
                        }
                    }
                    QueryGraphEdgeTransition::Downcast {
                        to_type_position, ..
                    } => {
                        format!("cannot find type \"{to_type_position}\"")
                    }
                    _ => {
                        bail!("Unhandled direct transition {}", transition);
                    }
                };
                Ok(Unadvanceables(Arc::new(vec![Arc::new(Unadvanceable {
                    reason: UnadvanceableReason::NoMatchingTransition,
                    from_subgraph: subgraph.clone(),
                    to_subgraph: subgraph.clone(),
                    details,
                })])))
            }))),
        ]))))
    }
}

impl TransitionPathWithLazyIndirectPaths {
    // PORT_NOTE: Named `initial()` in the JS codebase, but conventionally in Rust this kind of
    // constructor is named `new()`.
    pub(crate) fn new(path: Arc<TransitionGraphPath>) -> Self {
        Self {
            path,
            lazily_computed_indirect_paths: None,
        }
    }

    fn indirect_options(
        &mut self,
        condition_resolver: &mut impl ConditionResolver,
        override_conditions: &OverrideConditions,
    ) -> Result<TransitionIndirectPaths, FederationError> {
        if let Some(indirect_paths) = &self.lazily_computed_indirect_paths {
            Ok(indirect_paths.clone())
        } else {
            let new_indirect_paths =
                self.compute_indirect_paths(condition_resolver, override_conditions)?;
            self.lazily_computed_indirect_paths = Some(new_indirect_paths.clone());
            Ok(new_indirect_paths)
        }
    }

    fn compute_indirect_paths(
        &self,
        condition_resolver: &mut impl ConditionResolver,
        override_conditions: &OverrideConditions,
    ) -> Result<TransitionIndirectPaths, FederationError> {
        self.path
            .advance_with_non_collecting_and_type_preserving_transitions(
                &Default::default(),
                condition_resolver,
                &Default::default(),
                &Default::default(),
                override_conditions,
                // The transitions taken by this method are non-collecting transitions, in which case
                // the trigger is the context (which is really a hack to provide context information for
                // keys during fetch dependency graph updating).
                |transition, _| transition.clone(),
                |graph, node, trigger, override_conditions| {
                    graph.edge_for_transition_graph_path_trigger(node, trigger, override_conditions)
                },
                &Default::default(),
            )
    }

    /// Returns an `UnadvanceableClosures` if there is no way to advance the path with this
    /// transition. Otherwise, it returns a list of options (paths) we can be in after advancing the
    /// transition. The lists of options can be empty, which has the special meaning that the
    /// transition is guaranteed to have no results (due to unsatisfiable type conditions), meaning
    /// that as far as composition validation goes, we can ignore that transition (and anything that
    /// follows) and otherwise continue.
    // PORT_NOTE: In the JS codebase, this was named `advancePathWithTransition`.
    pub(crate) fn advance_with_transition(
        &mut self,
        transition: &QueryGraphEdgeTransition,
        target_type: &OutputTypeDefinitionPosition,
        api_schema: &ValidFederationSchema,
        condition_resolver: &mut impl ConditionResolver,
        override_conditions: &Arc<OverrideConditions>,
    ) -> Result<
        Either<Vec<TransitionPathWithLazyIndirectPaths>, UnadvanceableClosures>,
        FederationError,
    > {
        // The `transition` comes from the supergraph. Now, it is possible that a transition can be
        // expressed on the supergraph, but corresponds to an "unsatisfiable" condition on the
        // subgraph. Let's consider:
        // - Subgraph A:
        //    type Query {
        //       get: [I]
        //    }
        //
        //    interface I {
        //      k: Int
        //    }
        //
        //    type T1 implements I @key(fields: "k") {
        //      k: Int
        //      a: String
        //    }
        //
        //    type T2 implements I @key(fields: "k") {
        //      k: Int
        //      b: String
        //    }
        //
        // - Subgraph B:
        //    interface I {
        //      k: Int
        //    }
        //
        //    type T1 implements I @key(fields: "k") {
        //      k: Int
        //      myself: I
        //    }
        //
        // On the resulting supergraph, we will have a path for:
        //   {
        //     get {
        //       ... on T1 {
        //         myself {
        //           ... on T2 {
        //             b
        //           }
        //         }
        //       }
        //     }
        //   }
        //
        // As we compute possible subgraph paths, the `myself` field will get us in subgraph `B`
        // through `T1`'s key. However, as we look at transition `... on T2` from subgraph `B`, we
        // have no such type/transition. But this does not mean that the subgraphs shouldn't
        // compose. What it really means is that the corresponding query above can be done, but is
        // guaranteed to never return anything (essentially, we can query subgraph `B` but will
        // never get a `T2`, so the result of the query should be empty).
        //
        // So we need to handle this case, and we do this first. Note that the only kind of
        // transition that can give use this is a 'DownCast' transition. Also note that if the
        // subgraph type we're on is an @interfaceObject type, then we also can't be in this
        // situation as an @interfaceObject type "stands in" for all the possible implementations of
        // that interface. And one way to detect if the subgraph type an @interfaceObject is to
        // check if the subgraph type is an object type while the supergraph type is an interface
        // one.
        if let QueryGraphEdgeTransition::Downcast {
            from_type_position, ..
        } = &transition
        {
            let tail_weight = self.path.graph.node_weight(self.path.tail)?;
            let QueryGraphNodeType::SchemaType(tail_type_pos) = &tail_weight.type_ else {
                bail!("Subgraph path tail was unexpectedly a federation root node");
            };
            if !(matches!(
                from_type_position,
                CompositeTypeDefinitionPosition::Interface(_)
            ) && matches!(tail_type_pos, OutputTypeDefinitionPosition::Object(_)))
            {
                // If we consider a "downcast" transition, it means that the target of that cast is
                // composite, but also that the last type of the subgraph path is also composite
                // (that type is essentially the "source" of the cast).
                let target_type: CompositeTypeDefinitionPosition =
                    target_type.clone().try_into()?;
                let runtime_types_in_supergraph =
                    api_schema.possible_runtime_types(target_type.clone())?;
                let runtime_types_in_subgraph = &self.path.runtime_types_of_tail;
                // If the intersection is empty, it means whatever field got us here can never
                // resolve into an object of the type we're casting into. Essentially, we'll be fine
                // building a plan for this transition and whatever comes next: it'll just return
                // nothing.
                if runtime_types_in_subgraph.is_disjoint(&runtime_types_in_supergraph) {
                    debug!(
                        "No intersection between casted type {} and the possible types in this subgraph",
                        target_type,
                    );
                    return Ok(Either::Left(vec![]));
                }
            }
        }

        debug!("Trying to advance {} for {}", self.path, transition,);
        let span = debug_span!(" |");
        let options_guard = span.enter();
        debug!("Direct options:");
        let span = debug_span!(" |");
        let direct_options_guard = span.enter();
        let direct_options = self.path.advance_with_transition(
            transition,
            condition_resolver,
            override_conditions,
        )?;
        let mut options: Vec<Arc<TransitionGraphPath>> = vec![];
        let mut dead_end_closures: Vec<UnadvanceableClosure> = vec![];
        match direct_options {
            Either::Left(direct_options) => {
                drop(direct_options_guard);
                debug!("{}", DisplaySlice(&direct_options));
                // If we can fulfill the transition directly (without taking an edge) and the target
                // type is "terminal", then there is no point in computing all the options.
                if !direct_options.is_empty() && target_type.is_leaf_type() {
                    drop(options_guard);
                    debug!(
                        "reached leaf type {} so not trying indirect paths",
                        target_type,
                    );
                    return Ok(Either::Left(
                        direct_options
                            .into_iter()
                            .map(TransitionPathWithLazyIndirectPaths::new)
                            .collect(),
                    ));
                }
                options = direct_options;
            }
            Either::Right(closures) => {
                drop(direct_options_guard);
                debug!("No direct options");
                dead_end_closures = Arc::unwrap_or_clone(closures.0)
            }
        }
        // Otherwise, let's try non-collecting edges and see if we can find some (more) options
        // there.
        debug!("Computing indirect paths:");
        let span = debug_span!(" |");
        let indirect_options_guard = span.enter();
        let paths_with_non_collecting_edges =
            self.indirect_options(condition_resolver, override_conditions)?;
        if !paths_with_non_collecting_edges.paths.is_empty() {
            drop(indirect_options_guard);
            debug!(
                "{} indirect paths: {}",
                paths_with_non_collecting_edges.paths.len(),
                DisplaySlice(&paths_with_non_collecting_edges.paths),
            );
            debug!("Validating indirect options:");
            let span = debug_span!(" |");
            let _guard = span.enter();
            for non_collecting_path in paths_with_non_collecting_edges.paths.iter() {
                debug!("For indirect path {}:", non_collecting_path,);
                let span = debug_span!(" |");
                let indirect_option_guard = span.enter();
                let paths_with_transition = non_collecting_path.advance_with_transition(
                    transition,
                    condition_resolver,
                    override_conditions,
                )?;
                match paths_with_transition {
                    Either::Left(mut paths_with_transition) => {
                        drop(indirect_option_guard);
                        debug!(
                            "Adding valid option: {}",
                            DisplaySlice(&paths_with_transition)
                        );
                        options.append(&mut paths_with_transition);
                    }
                    Either::Right(closures) => {
                        drop(indirect_option_guard);
                        debug!("Cannot be advanced with {}", transition);
                        dead_end_closures.extend(closures.0.iter().cloned());
                    }
                }
            }
        } else {
            drop(indirect_options_guard);
            debug!("no indirect paths");
        }
        if !options.is_empty() {
            drop(options_guard);
            debug!("{}", DisplaySlice(&options));
            return Ok(Either::Left(
                options
                    .into_iter()
                    .map(TransitionPathWithLazyIndirectPaths::new)
                    .collect(),
            ));
        } else {
            drop(options_guard);
            debug!("Cannot advance {} for this path", transition);
        }

        let transition = transition.clone();
        let graph = self.path.graph.clone();
        let tail = self.path.tail;
        let indirect_dead_end_closures = paths_with_non_collecting_edges.dead_ends.0;
        Ok(Either::Right(UnadvanceableClosures(Arc::new(vec![
            Arc::new(LazyLock::new(Box::new(move || {
                dead_end_closures.extend(indirect_dead_end_closures.iter().cloned());
                let mut all_dead_ends: Vec<Arc<Unadvanceable>> = Arc::unwrap_or_clone(
                    Unadvanceables::try_from(UnadvanceableClosures(Arc::new(dead_end_closures)))?.0,
                );
                if let QueryGraphEdgeTransition::FieldCollection {
                    field_definition_position,
                    ..
                } = &transition
                {
                    let parent_type_pos = field_definition_position.parent();
                    let tail_weight = graph.node_weight(tail)?;
                    let QueryGraphNodeType::SchemaType(tail_type_pos) = &tail_weight.type_ else {
                        bail!("Subgraph path tail was unexpectedly a federation root node");
                    };
                    let subgraphs_with_dead_ends = all_dead_ends
                        .iter()
                        .map(|unadvanceable| unadvanceable.to_subgraph.clone())
                        .collect::<IndexSet<_>>();
                    for (subgraph, subgraph_schema) in graph.subgraphs() {
                        if subgraphs_with_dead_ends.contains(subgraph) {
                            continue;
                        }
                        let parent_type_pos_in_subgraph =
                            subgraph_schema.try_get_type(parent_type_pos.type_name().clone());
                        let Some(Ok(parent_type_pos_in_subgraph)) = parent_type_pos_in_subgraph
                            .map(CompositeTypeDefinitionPosition::try_from)
                        else {
                            continue;
                        };
                        let Ok(field_pos_in_subgraph) = parent_type_pos_in_subgraph
                            .field(field_definition_position.field_name().clone())
                        else {
                            continue;
                        };
                        let Ok(_field_in_subgraph) =
                            field_pos_in_subgraph.get(subgraph_schema.schema())
                        else {
                            continue;
                        };
                        // The subgraph has the type we're looking for, but we have recorded no
                        // dead-ends. This means there is no edge to that type, and thus either:
                        // 1. it has no keys, or
                        // 2. the path to advance it is an @interfaceObject type, the type we look
                        //    for is an implementation of that interface, and there are no keys on
                        //    the interface.
                        let tail_type_pos_in_subgraph =
                            subgraph_schema.try_get_type(tail_type_pos.type_name().clone());
                        if let Some(tail_type_pos_in_subgraph) = tail_type_pos_in_subgraph {
                            // `tail_type_pos_in_subgraph` exists, so it's either equal to
                            // `parent_type_pos_in_subgraph`, or it's an interface of it. In any
                            // case, it's composite.
                            let Ok(tail_type_pos_in_subgraph) =
                                CompositeTypeDefinitionPosition::try_from(
                                    tail_type_pos_in_subgraph.clone(),
                                )
                            else {
                                bail!(
                                    "Type {} in {} should be composite",
                                    tail_type_pos_in_subgraph,
                                    subgraph,
                                );
                            };
                            let Some(metadata) = subgraph_schema.subgraph_metadata() else {
                                bail!("Type should originate from a federation subgraph schema");
                            };
                            let key_directive_definition_name = &metadata
                                .federation_spec_definition()
                                .key_directive_definition(subgraph_schema)?
                                .name;
                            let tail_type_def =
                                tail_type_pos_in_subgraph.get(subgraph_schema.schema())?;
                            let key_resolvables = tail_type_def
                                .directives()
                                .get_all(key_directive_definition_name)
                                .map(|application| {
                                    Ok::<_, FederationError>(
                                        metadata
                                            .federation_spec_definition()
                                            .key_directive_arguments(application)?
                                            .resolvable,
                                    )
                                })
                                .process_results(|iter| iter.collect::<Vec<_>>())?;
                            ensure!(
                                key_resolvables.iter().all(Not::not),
                                "After transition {}, expected type {} in {} to have no resolvable keys",
                                transition,
                                parent_type_pos_in_subgraph,
                                subgraph,
                            );
                            let kind_of_type =
                                if tail_type_pos_in_subgraph == parent_type_pos_in_subgraph {
                                    "type"
                                } else {
                                    "interface"
                                };
                            let explanation = if key_resolvables.is_empty() {
                                format!(
                                    "{kind_of_type} \"{tail_type_pos}\" has no @key defined in subgraph \"{subgraph}\"",
                                )
                            } else {
                                format!(
                                    "none of the @key defined on {kind_of_type} \"{tail_type_pos}\" in subgraph \"{subgraph}\" are resolvable (they are all declared with their \"resolvable\" argument set to false)",
                                )
                            };
                            all_dead_ends.push(Arc::new(Unadvanceable {
                                reason: UnadvanceableReason::UnreachableType,
                                from_subgraph: tail_weight.source.clone(),
                                to_subgraph: subgraph.clone(),
                                details: format!(
                                    "cannot move to subgraph \"{subgraph}\", which has field \"{field_definition_position}\", because {explanation}",
                                ),
                            }))
                        } else {
                            // This means that:
                            // 1. the type of the path we're trying to advance is different from the
                            //    transition we're considering, and that should only happen if the
                            //    path is on an @interfaceObject type, and
                            // 2. the subgraph we're looking at actually doesn't have that interface
                            //    (to be able to jump to that subgraph, we would need the interface
                            //    and it would need to have a resolvable key, but it has neither).
                            all_dead_ends.push(Arc::new(Unadvanceable {
                                reason: UnadvanceableReason::UnreachableType,
                                from_subgraph: tail_weight.source.clone(),
                                to_subgraph: subgraph.clone(),
                                details: format!(
                                    "cannot move to subgraph \"{subgraph}\", which has field \"{field_definition_position}\", because interface \"{tail_type_pos}\" is not defined in this subgraph (to jump to \"{subgraph}\", it would need to both define interface \"{tail_type_pos}\" and have a @key on it)",
                                ),
                            }))
                        }
                    }
                }

                Ok(Unadvanceables(Arc::new(all_dead_ends)))
            }))),
        ]))))
    }
}
