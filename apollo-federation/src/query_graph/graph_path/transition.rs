use std::borrow::Cow;
use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use either::Either;
use itertools::Itertools;
use petgraph::graph::EdgeIndex;

use crate::bail;
use crate::ensure;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::Selection;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::condition_resolver::ConditionResolver;
use crate::query_graph::condition_resolver::UnsatisfiedConditionReason;
use crate::query_graph::graph_path::GraphPath;
use crate::query_graph::graph_path::GraphPathTriggerVariant;
use crate::query_graph::graph_path::Unadvanceable;
use crate::query_graph::graph_path::UnadvanceableClosure;
use crate::query_graph::graph_path::UnadvanceableClosures;
use crate::query_graph::graph_path::UnadvanceableReason;
use crate::query_graph::graph_path::Unadvanceables;
use crate::query_plan::query_planner::EnabledOverrideConditions;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
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
            " (please ensure that this is not due to key {} being accidentally marked @external)",
            fields_list
        ))
    }

    // PORT_NOTE: Named `findOverriddingSourcesIfOverridden()` in the JS codebase, but we've changed
    // sources to subgraphs here for clarity that we don't need to handle the federated root source.
    fn find_overridding_subgraphs_if_overridden(
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
    #[allow(dead_code)]
    fn advance_with_transition(
        &self,
        transition: &QueryGraphEdgeTransition,
        condition_resolver: &mut impl ConditionResolver,
        override_conditions: &EnabledOverrideConditions,
    ) -> Result<Either<Vec<TransitionGraphPath>, UnadvanceableClosures>, FederationError> {
        ensure!(
            transition.collect_operation_elements(),
            "Supergraphs shouldn't have transitions that don't collect elements",
        );
        let mut to_advance = Cow::Borrowed(self);
        if let QueryGraphEdgeTransition::FieldCollection {
            source,
            field_definition_position,
            ..
        } = transition
        {
            let tail_weight = self.graph.node_weight(self.tail)?;
            if let Ok(tail_type) = <QueryGraphNodeType as TryInto<
                CompositeTypeDefinitionPosition,
            >>::try_into(tail_weight.type_.clone())
            {
                if field_definition_position.parent().type_name() != tail_type.type_name()
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
                    to_advance = Cow::Owned(updated_path);
                    // We can now continue on dealing with the actual field.
                }
            }
        }

        let mut options: Vec<TransitionGraphPath> = vec![];
        let mut dead_end_closures: Vec<UnadvanceableClosure> = vec![];

        for edge in to_advance.next_edges()? {
            let edge_weight = to_advance.graph.edge_weight(edge)?;
            // The edge must match the transition. If it doesn't, we cannot use it.
            if !edge_weight
                .transition
                .matches_supergraph_transition(transition)?
            {
                continue;
            }

            if let Some(override_condition) = &edge_weight.override_condition {
                if !edge_weight.satisfies_override_conditions(override_conditions) {
                    let graph = to_advance.graph.clone();
                    let override_condition = override_condition.clone();
                    let override_conditions = override_conditions.clone();
                    dead_end_closures.push(Box::new(move || {
                        let edge_weight = graph.edge_weight(edge)?;
                        let (head, tail) = graph.edge_endpoints(edge)?;
                        let head_weight = graph.node_weight(head)?;
                        let tail_weight = graph.node_weight(tail)?;
                        Ok(Unadvanceables(vec![Unadvanceable {
                            reason: UnadvanceableReason::UnsatisfiableOverrideCondition,
                            from_subgraph: head_weight.source.clone(),
                            to_subgraph: tail_weight.source.clone(),
                            details: format!(
                                "Unable to take edge {} because override condition \"{}\" is {}",
                                edge_weight,
                                override_condition.label,
                                override_conditions.contains(&override_condition.label)
                            ),
                        }]))
                    }));
                    continue;
                }
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
                    options.push(to_advance.add(
                        transition.clone(),
                        edge,
                        condition_resolution,
                        None,
                    )?);
                }
                ConditionResolution::Unsatisfied { reason } => {
                    let transition = transition.clone();
                    let graph = to_advance.graph.clone();
                    dead_end_closures.push(Box::new(move || {
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
                                            "could not find a match for required context for field \"{}\"",
                                            field_definition_position,
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
                                Ok(Unadvanceables(vec![Unadvanceable {
                                    reason: UnadvanceableReason::UnsatisfiableRequiresCondition,
                                    from_subgraph: head_weight.source.clone(),
                                    to_subgraph: head_weight.source.clone(),
                                    details,
                                }]))
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
                                Ok(Unadvanceables(vec![Unadvanceable {
                                    reason: UnadvanceableReason::UnresolvableInterfaceObject,
                                    from_subgraph: head_weight.source.clone(),
                                    to_subgraph: head_weight.source.clone(),
                                    details,
                                }]))
                            }
                            _ => {
                                bail!(
                                "Shouldn't have conditions on direct transition {}",
                                transition,
                            );
                            }
                        }
                    }));
                }
            }
        }

        if !options.is_empty() {
            return Ok(Either::Left(options));
        }

        let transition = transition.clone();
        let graph = self.graph.clone();
        let tail = self.tail;
        Ok(Either::Right(UnadvanceableClosures(vec![Box::new(
            move || {
                let dead_ends: Unadvanceables =
                    UnadvanceableClosures(dead_end_closures).try_into()?;
                if !dead_ends.0.is_empty() {
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
                                "cannot find implementation type \"{}\" (supergraph interface \"{}\" is declared with @interfaceObject in \"{}\")",
                                parent_type_pos, tail_type_pos, subgraph,
                            );
                        }
                        let Some(Ok::<CompositeTypeDefinitionPosition, _>(
                            parent_type_pos_in_subgraph,
                        )) = parent_type_pos_in_subgraph.map(|pos| pos.try_into())
                        else {
                            break 'details format!(
                                "cannot find field \"{}\"",
                                field_definition_position,
                            );
                        };
                        let Ok(field_pos_in_subgraph) = parent_type_pos_in_subgraph
                            .field(field_definition_position.field_name().clone())
                        else {
                            break 'details format!(
                                "cannot find field \"{}\"",
                                field_definition_position,
                            );
                        };
                        let Ok(field_in_subgraph) =
                            field_pos_in_subgraph.get(subgraph_schema.schema())
                        else {
                            break 'details format!(
                                "cannot find field \"{}\"",
                                field_definition_position,
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
                        let external_reason = metadata
                            .federation_spec_definition()
                            .external_directive_arguments(application)?
                            .reason;
                        let overriding_subgraphs = if external_reason == Some("[overridden]") {
                            Self::find_overridding_subgraphs_if_overridden(
                                &field_pos_in_subgraph,
                                subgraph,
                                graph.subgraph_schemas(),
                            )?
                        } else {
                            vec![]
                        };
                        if overriding_subgraphs.is_empty() {
                            format!(
                                "field \"{}\" is not resolvable because marked @external",
                                field_definition_position,
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
                        format!("cannot find type \"{}\"", to_type_position)
                    }
                    _ => {
                        bail!("Unhandled direct transition {}", transition);
                    }
                };
                Ok(Unadvanceables(vec![Unadvanceable {
                    reason: UnadvanceableReason::NoMatchingTransition,
                    from_subgraph: subgraph.clone(),
                    to_subgraph: subgraph.clone(),
                    details,
                }]))
            },
        )])))
    }
}
