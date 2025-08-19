use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::link::federation_spec_definition::FEDERATION_CONTEXT_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FIELD_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_FROM_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_GRAPH_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_NAME_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_SELECTION_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_TYPE_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_USED_OVERRIDEN_ARGUMENT_NAME;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge::map_sources;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::validators::from_context::parse_context;
use crate::utils::human_readable::human_readable_subgraph_names;
use crate::utils::human_readable::human_readable_types;

#[derive(Debug, Clone)]
struct SubgraphWithIndex {
    subgraph: String,
    idx: usize,
}

#[derive(Debug, Clone)]
struct SubgraphField {
    subgraph: String,
    field: FieldDefinitionPosition,
}

trait HasSubgraph {
    fn subgraph(&self) -> &str;
}

impl HasSubgraph for SubgraphWithIndex {
    fn subgraph(&self) -> &str {
        &self.subgraph
    }
}

impl HasSubgraph for SubgraphField {
    fn subgraph(&self) -> &str {
        &self.subgraph
    }
}

impl Merger {
    #[allow(dead_code)]
    pub(crate) fn merge_field(
        &mut self,
        sources: &Sources<DirectiveTargetPosition>,
        dest: &DirectiveTargetPosition,
    ) -> Result<(), FederationError> {
        let every_source_is_external = sources.iter().all(|(i, source)| {
            let Some(metadata) = self.subgraphs.get(*i).map(|s| s.metadata()) else {
                // If subgraph not found, consider it not external to fail safely
                return false;
            };
            match source {
                None => self
                    .fields_in_source_if_abstracted_by_interface_object(dest, *i)
                    .iter()
                    .all(|f| {
                        let field_pos = match f {
                            DirectiveTargetPosition::ObjectField(pos) => {
                                FieldDefinitionPosition::Object(pos.clone())
                            }
                            DirectiveTargetPosition::InterfaceField(pos) => {
                                FieldDefinitionPosition::Interface(pos.clone())
                            }
                            _ => return false, // Input objects and other non-field positions are not external
                        };
                        metadata.external_metadata().is_external(&field_pos)
                    }),
                Some(s) => {
                    let field_pos = match s {
                        DirectiveTargetPosition::ObjectField(pos) => {
                            FieldDefinitionPosition::Object(pos.clone())
                        }
                        DirectiveTargetPosition::InterfaceField(pos) => {
                            FieldDefinitionPosition::Interface(pos.clone())
                        }
                        _ => return false, // Input objects and other non-field positions are not external
                    };
                    metadata.external_metadata().is_external(&field_pos)
                }
            }
        });

        if every_source_is_external {
            let defining_subgraphs: Vec<String> = sources
                .iter()
                .filter_map(|(i, source)| {
                    match source {
                        Some(_source_field) => {
                            // Direct field definition in this subgraph
                            Some(self.names[*i].clone())
                        }
                        None => {
                            // Check for interface object fields
                            let itf_object_fields =
                                self.fields_in_source_if_abstracted_by_interface_object(dest, *i);
                            if itf_object_fields.is_empty() {
                                return None;
                            }

                            // Build description for interface object abstraction
                            Some(format!(
                                "{} (through @interfaceObject {})",
                                self.names[*i],
                                human_readable_types(itf_object_fields.iter().map(|f| {
                                    match f {
                                        DirectiveTargetPosition::ObjectField(pos) => {
                                            pos.type_name.to_string()
                                        }
                                        DirectiveTargetPosition::InterfaceField(pos) => {
                                            pos.type_name.to_string()
                                        }
                                        DirectiveTargetPosition::InputObjectField(pos) => {
                                            pos.type_name.to_string()
                                        }
                                        _ => "unknown".to_string(),
                                    }
                                }))
                            ))
                        }
                    }
                })
                .collect();

            // Create and report composition error
            let error = CompositionError::ExternalMissingOnBase {
                message: format!(
                    "Field \"{}\" is marked @external on all the subgraphs in which it is listed ({}).",
                    dest,
                    defining_subgraphs.join(", ")
                ),
            };

            self.error_reporter.add_error(error);

            return Ok(());
        }

        let without_external = self.validate_and_filter_external(sources);

        // Note that we don't truly merge externals: we don't want, for instance, a field that is non-nullable everywhere to appear nullable in the
        // supergraph just because someone fat-fingered the type in an external definition. But after merging the non-external definitions, we
        // validate the external ones are consistent.

        self.merge_description(&without_external, dest);
        self.record_applied_directives_to_merge(&without_external, dest);
        self.add_arguments_shallow(&without_external, dest);
        let dest_field = match dest {
            DirectiveTargetPosition::ObjectField(pos) => pos.get(self.merged.schema())?,
            DirectiveTargetPosition::InterfaceField(pos) => pos.get(self.merged.schema())?,
            _ => {
                return Ok(()); // Skip input object fields and other non-field positions
            }
        };
        let dest_arguments = dest_field.arguments.clone();
        for dest_arg in dest_arguments.iter() {
            let subgraph_args = map_sources(&without_external, |field| {
                field.as_ref().and_then(|f| {
                    let field_def = match f {
                        DirectiveTargetPosition::ObjectField(pos) => {
                            pos.get(self.merged.schema()).ok()?
                        }
                        DirectiveTargetPosition::InterfaceField(pos) => {
                            pos.get(self.merged.schema()).ok()?
                        }
                        _ => return None, // Input objects and other positions don't have field arguments
                    };
                    field_def
                        .arguments
                        .iter()
                        .find(|arg| arg.name == dest_arg.name)
                        .cloned()
                })
            });
            self.merge_argument(&subgraph_args, dest_arg)?;
        }

        // Note that due to @interfaceObject, it's possible that `withoutExternal` is "empty" (has no
        // non-undefined at all) but to still get here. That is, we can have:
        // ```
        //   # First subgraph
        //   interface I {
        //     id: ID!
        //     x: Int
        //   }
        //
        //   type T implements I @key(fields: "id") {
        //     id: ID!
        //     x: Int @external
        //     y: Int @requires(fields: "x")
        //   }
        // ```
        // and
        // ```
        //   # Second subgraph
        //   type I @interfaceObject @key(fields: "id") {
        //     id: ID!
        //     x: Int
        //   }
        // ```
        // In that case, it is valid to mark `T.x` external because it is provided by
        // another subgraph, the second one, through the interfaceObject object on I.
        // But because the first subgraph is the only one to have `T` and `x` is
        // external there, `withoutExternal` will be false.
        //
        // Anyway, we still need to merge a type in the supergraph, so in that case
        // we use merge the external declarations directly.

        // Use sources with defined values if available, otherwise fall back to original sources
        // This mirrors the TypeScript logic: someSources(withoutExternal, isDefined) ? withoutExternal : sources
        let sources_for_type_merge =
            if Self::some_sources(&without_external, |source, _| source.is_some()) {
                &without_external
            } else {
                sources
            };

        // Transform FieldDefinitionPosition sources to Type sources
        let type_sources: Sources<Type> = sources_for_type_merge
            .iter()
            .map(|(idx, field_pos)| {
                let type_option = field_pos
                    .as_ref()
                    .and_then(|pos| match pos {
                        DirectiveTargetPosition::ObjectField(p) => p.get(self.merged.schema()).ok(),
                        DirectiveTargetPosition::InterfaceField(p) => {
                            p.get(self.merged.schema()).ok()
                        }
                        _ => None, // Input objects and other positions don't participate in type merging
                    })
                    .map(|field_def| field_def.ty.clone());
                (*idx, type_option)
            })
            .collect();

        // Get mutable access to the dest field definition for type merging
        let dest_field_component = match dest {
            DirectiveTargetPosition::ObjectField(pos) => pos.get(self.merged.schema())?,
            DirectiveTargetPosition::InterfaceField(pos) => pos.get(self.merged.schema())?,
            _ => {
                return Ok(()); // Skip input object fields and other non-field positions
            }
        }
        .clone();
        let mut dest_field_ast = dest_field_component.as_ref().clone();
        let dest_parent_type_name = match dest {
            DirectiveTargetPosition::ObjectField(pos) => &pos.type_name,
            DirectiveTargetPosition::InterfaceField(pos) => &pos.type_name,
            DirectiveTargetPosition::InputObjectField(pos) => &pos.type_name,
            _ => {
                bail!("Invalid field position for parent extraction: {:?}", dest);
            }
        };
        let all_types_equal = self.merge_type_reference(
            &type_sources,
            &mut dest_field_ast,
            false,
            dest_parent_type_name,
        )?;

        if self.has_external(sources) {
            // Convert to FieldDefinitionPosition types for external field validation
            let field_sources: Sources<FieldDefinitionPosition> = sources
                .iter()
                .map(|(idx, source)| {
                    match source {
                        Some(DirectiveTargetPosition::ObjectField(pos)) => {
                            (*idx, Some(FieldDefinitionPosition::Object(pos.clone())))
                        }
                        Some(DirectiveTargetPosition::InterfaceField(pos)) => {
                            (*idx, Some(FieldDefinitionPosition::Interface(pos.clone())))
                        }
                        Some(_) => (*idx, None), // Non-field positions become None
                        None => (*idx, None),
                    }
                })
                .collect();

            let field_dest = match dest {
                DirectiveTargetPosition::ObjectField(pos) => {
                    FieldDefinitionPosition::Object(pos.clone())
                }
                DirectiveTargetPosition::InterfaceField(pos) => {
                    FieldDefinitionPosition::Interface(pos.clone())
                }
                _ => return Ok(()), // Skip validation for non-field positions
            };

            self.validate_external_fields(&field_sources, &field_dest, all_types_equal)?;
        }
        // Create a default merge context for basic field merging
        // (advanced override scenarios would provide a more sophisticated context)
        let merge_context = FieldMergeContext::default();
        self.add_join_field(sources, dest, all_types_equal, &merge_context)?;
        self.add_join_directive_directives(sources, dest)?;
        Ok(())
    }

    fn fields_in_source_if_abstracted_by_interface_object(
        &self,
        dest_field: &DirectiveTargetPosition,
        source_idx: usize,
    ) -> Vec<DirectiveTargetPosition> {
        // Get the parent type of the destination field
        let parent_in_supergraph = match dest_field {
            DirectiveTargetPosition::ObjectField(field) => {
                CompositeTypeDefinitionPosition::Object(field.parent())
            }
            DirectiveTargetPosition::InterfaceField(field) => {
                CompositeTypeDefinitionPosition::Interface(field.parent())
            }
            _ => return Vec::new(), // Input objects and other positions can't be abstracted by interface objects
        };

        // Check if parent is an object type, if not or if it exists in the source schema, return empty
        if !parent_in_supergraph.is_object_type() {
            return Vec::new();
        }

        let subgraph_schema = self.subgraphs[source_idx].validated_schema().schema();
        if subgraph_schema
            .types
            .contains_key(parent_in_supergraph.type_name())
        {
            return Vec::new();
        }

        let field_name = match dest_field {
            DirectiveTargetPosition::ObjectField(pos) => &pos.field_name,
            DirectiveTargetPosition::InterfaceField(pos) => &pos.field_name,
            DirectiveTargetPosition::InputObjectField(pos) => &pos.field_name,
            _ => {
                return Vec::new(); // Skip non-field positions
            }
        };

        let parent_object = match dest_field {
            DirectiveTargetPosition::ObjectField(pos) => {
                CompositeTypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                    type_name: pos.type_name.clone(),
                })
            }
            DirectiveTargetPosition::InterfaceField(pos) => {
                CompositeTypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
                    type_name: pos.type_name.clone(),
                })
            }
            _ => {
                return Vec::new(); // Input objects and other non-composite field types don't have interfaces
            }
        };

        // Get the object type from the supergraph to access its implemented interfaces
        let Ok(composite_type) = parent_object.get(self.merged.schema()) else {
            return Vec::new();
        };

        // Extract implements_interfaces from the composite type
        let implements_interfaces = match composite_type {
            ExtendedType::Object(obj) => &obj.implements_interfaces,
            ExtendedType::Interface(iface) => &iface.implements_interfaces,
            _ => {
                return Vec::new(); // Union types don't have implements_interfaces
            }
        };

        // Find interface object fields that provide this field
        implements_interfaces
            .iter()
            .filter_map(|interface_name| {
                // Get the interface type from the supergraph
                let interface_type = self.merged.schema().types.get(&interface_name.name)?;
                let ExtendedType::Interface(interface_def) = interface_type else {
                    return None; // Skip if not an interface type
                };

                // Check if this interface field exists
                if !interface_def.fields.contains_key(field_name) {
                    return None;
                }

                // Note that since the type is an interface in the supergraph, we can assume that
                // if it is an object type in the subgraph, then it is an @interfaceObject.
                let type_in_subgraph = subgraph_schema.types.get(&interface_name.name)?;

                // If it's an object type in the subgraph (while being an interface in supergraph),
                // it must be an @interfaceObject
                if let ExtendedType::Object(obj_type) = type_in_subgraph {
                    // Check if the object type has the field
                    if obj_type.fields.contains_key(field_name) {
                        Some(DirectiveTargetPosition::ObjectField(
                            ObjectFieldDefinitionPosition {
                                type_name: interface_name.name.clone(),
                                field_name: field_name.clone(),
                            },
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }

    fn validate_and_filter_external(
        &mut self,
        sources: &Sources<DirectiveTargetPosition>,
    ) -> Sources<DirectiveTargetPosition> {
        sources
            .iter()
            .fold(Sources::default(), |mut filtered, (i, source)| {
                match source {
                    // If no source or not external, mirror the input
                    None => {
                        filtered.insert(*i, source.clone());
                        filtered
                    }
                    Some(field_pos)
                        if !match field_pos {
                            DirectiveTargetPosition::ObjectField(pos) => self.is_field_external(
                                *i,
                                &FieldDefinitionPosition::Object(pos.clone()),
                            ),
                            DirectiveTargetPosition::InterfaceField(pos) => self.is_field_external(
                                *i,
                                &FieldDefinitionPosition::Interface(pos.clone()),
                            ),
                            _ => false, // Non-field positions can't be external
                        } =>
                    {
                        filtered.insert(*i, source.clone());
                        filtered
                    }
                    // External field: filter out but validate directives
                    Some(field_pos) => {
                        filtered.insert(*i, None);

                        // Validate that external fields don't have merged directives
                        // We don't allow "merged" directives on external fields because as far as merging goes, external fields don't really
                        // exist and allowing "merged" directives on them is dodgy. To take examples, having a `@deprecated` or `@tag` on
                        // an external feels unclear semantically: should it deprecate/tag the field? Essentially we're saying that "no it
                        // shouldn't" and so it's clearer to reject it.
                        // Note that if we change our mind on this semantic and wanted directives on external to propagate, then we'll also
                        // need to update the merging of fields since external fields are filtered out (by this very method).

                        // Convert DirectiveTargetPosition to FieldDefinitionPosition for @external validation
                        if let Some(field_def_pos) = match field_pos {
                            DirectiveTargetPosition::ObjectField(pos) => {
                                Some(FieldDefinitionPosition::Object(pos.clone()))
                            }
                            DirectiveTargetPosition::InterfaceField(pos) => {
                                Some(FieldDefinitionPosition::Interface(pos.clone()))
                            }
                            _ => None, // @external only applies to object and interface fields
                        } {
                            // Ignore validation result: validation errors are reported via error_reporter,
                            // and field lookup failures are treated as non-fatal during filtering
                            let _ = self.validate_external_field_directives(*i, &field_def_pos);
                        }
                        filtered
                    }
                }
            })
    }

    #[allow(dead_code)]
    fn validate_external_field_directives(
        &mut self,
        source_idx: usize,
        field_pos: &FieldDefinitionPosition,
    ) -> Result<(), FederationError> {
        // Get the field definition to check its directives
        let field_def = field_pos.get(self.subgraphs[source_idx].validated_schema().schema())?;

        // Check each directive for violations
        for directive in &field_def.directives {
            if self.is_merged_directive(&self.names[source_idx], directive) {
                // Contrarily to most of the errors during merging that "merge" errors for related elements, we're logging one
                // error for every application here. But this is because the error is somewhat subgraph specific and is
                // unlikely to span multiple subgraphs. In fact, we could almost have thrown this error during subgraph validation
                // if this wasn't for the fact that it is only thrown for directives being merged and so is more logical to
                // be thrown only when merging.

                let error = CompositionError::MergedDirectiveApplicationOnExternal {
                    message: format!(
                        "Cannot apply merged directive @{} to external field \"{}\" (in subgraph \"{}\")",
                        directive.name, field_pos, self.names[source_idx]
                    ),
                };

                self.error_reporter.add_error(error);
            }
        }

        Ok(())
    }

    fn is_field_external(&self, source_idx: usize, field: &FieldDefinitionPosition) -> bool {
        // Use the subgraph metadata to check if field is external
        self.subgraphs[source_idx]
            .metadata()
            .is_field_external(field)
    }

    /// Check if any of the provided sources contains external fields
    /// Uses some_sources for efficient checking
    fn has_external(&self, sources: &Sources<DirectiveTargetPosition>) -> bool {
        Self::some_sources(sources, |source, idx| match source {
            Some(field_pos) => match field_pos {
                DirectiveTargetPosition::ObjectField(pos) => {
                    self.is_field_external(idx, &FieldDefinitionPosition::Object(pos.clone()))
                }
                DirectiveTargetPosition::InterfaceField(pos) => {
                    self.is_field_external(idx, &FieldDefinitionPosition::Interface(pos.clone()))
                }
                _ => false, // Non-field positions can't be external
            },
            None => false,
        })
    }

    /// Validate external field constraints across subgraphs
    fn validate_external_fields(
        &mut self,
        sources: &Sources<FieldDefinitionPosition>,
        dest: &FieldDefinitionPosition,
        all_types_equal: bool,
    ) -> Result<(), FederationError> {
        // Get the destination field definition for validation
        let dest_field = dest.get(self.merged.schema())?;
        let dest_field_ty = dest_field.ty.clone();
        let dest_args = dest_field.arguments.to_vec();

        // Phase 1: Collection - collect all error types into separate sets
        let mut has_invalid_types = false;
        let mut invalid_args_presence = HashSet::new();
        let mut invalid_args_types = HashSet::new();
        let mut invalid_args_defaults = HashSet::new();

        for (source_idx, source) in sources.iter() {
            let Some(source_field_pos) = source else {
                continue;
            };

            // Only validate external fields
            if !self.is_field_external(*source_idx, source_field_pos) {
                continue;
            }

            let source_field =
                source_field_pos.get(self.subgraphs[*source_idx].validated_schema().schema())?;
            let source_args = source_field.arguments.to_vec();

            // To be valid, an external field must use the same type as the merged field (or "at least" a subtype).
            let is_subtype = if all_types_equal {
                false
            } else {
                self.is_strict_subtype(&dest_field_ty, &source_field.ty)
                    .unwrap_or(false)
            };

            if !(dest_field_ty == source_field.ty || is_subtype) {
                has_invalid_types = true;
            }

            // For arguments, it should at least have all the arguments of the merged, and their type needs to be supertypes (contravariance).
            // We also require the default is that of the supergraph (maybe we could relax that, but we should decide how we want
            // to deal with field with arguments in @key, @provides, @requires first as this could impact it).
            for dest_arg in &dest_args {
                let name = &dest_arg.name;
                let Some(source_arg) = source_args.iter().find(|arg| &arg.name == name) else {
                    invalid_args_presence.insert(name.clone());
                    continue;
                };
                let arg_is_subtype = self
                    .is_strict_subtype(&source_arg.ty, &dest_arg.ty)
                    .unwrap_or(false);
                if dest_arg.ty != source_arg.ty && !arg_is_subtype {
                    invalid_args_types.insert(name.clone());
                }
                // TODO: Use valueEquals instead of != for proper GraphQL value comparison
                // See: https://github.com/apollographql/federation/blob/4653320016ed4202a229d9ab5933ad3f13e5b6c0/composition-js/src/merging/merge.ts#L1877
                if dest_arg.default_value != source_arg.default_value {
                    invalid_args_defaults.insert(name.clone());
                }
            }
        }

        // Phase 2: Reporting - report errors in groups, matching JS version order
        if has_invalid_types {
            self.error_reporter.report_mismatch_error::<FieldDefinitionPosition, ()>(
                CompositionError::ExternalTypeMismatch {
                    message: format!(
                        "Type of field \"{}\" is incompatible across subgraphs (where marked @external): it has ",
                        dest,
                    ),
                },
                dest,
                sources,
                |source, _| Some(format!("type \"{}\"", source)),
            );
        }

        for arg_name in &invalid_args_presence {
            self.report_mismatch_error_with_specifics(
                CompositionError::ExternalArgumentMissing {
                    message: format!(
                        "Field \"{}\" is missing argument \"{}\" in some subgraphs where it is marked @external: ",
                        dest, arg_name
                    ),
                },
                &self.argument_sources(sources, arg_name)?,
                |source| source.as_ref().map_or("", |_| ""),
            );
        }

        for arg_name in &invalid_args_types {
            let argument_pos = ObjectFieldArgumentDefinitionPosition {
                type_name: dest.type_name().clone(),
                field_name: dest.field_name().clone(),
                argument_name: arg_name.clone(),
            };
            self.error_reporter.report_mismatch_error::<ObjectFieldArgumentDefinitionPosition, ()>(
                CompositionError::ExternalArgumentTypeMismatch {
                    message: format!(
                        "Type of argument \"{}\" is incompatible across subgraphs (where \"{}\" is marked @external): it has ",
                        argument_pos,
                        dest,
                    ),
                },
                &argument_pos,
                &self.argument_sources(sources, arg_name)?,
                |source, _| Some(format!("type \"{}\"", source)),
            );
        }

        for arg_name in &invalid_args_defaults {
            let argument_pos = ObjectFieldArgumentDefinitionPosition {
                type_name: dest.type_name().clone(),
                field_name: dest.field_name().clone(),
                argument_name: arg_name.clone(),
            };
            self.error_reporter.report_mismatch_error::<ObjectFieldArgumentDefinitionPosition, ()>(
                CompositionError::ExternalArgumentDefaultMismatch {
                    message: format!(
                        "Argument \"{}\" has incompatible defaults across subgraphs (where \"{}\" is marked @external): it has ",
                        argument_pos,
                        dest,
                    ),
                },
                &argument_pos,
                &self.argument_sources(sources, arg_name)?,
                |source, _| Some(format!("default value {:?}", source)), // TODO: Need proper value formatting
            );
        }

        Ok(())
    }

    /// Create argument sources from field sources for a specific argument
    /// Transforms Sources<FieldDefinitionPosition> into Sources<ObjectFieldArgumentDefinitionPosition>
    fn argument_sources(
        &self,
        sources: &Sources<FieldDefinitionPosition>,
        dest_arg_name: &Name,
    ) -> Result<Sources<ObjectFieldArgumentDefinitionPosition>, FederationError> {
        let mut arg_sources = Sources::default();

        for (source_idx, source_field_pos) in sources.iter() {
            let arg_position = if let Some(field_pos) = source_field_pos {
                // Get the field definition to check if it has the argument
                let field_def =
                    field_pos.get(self.subgraphs[*source_idx].validated_schema().schema())?;

                // Check if the field has this argument
                if field_def
                    .arguments
                    .iter()
                    .any(|arg| &arg.name == dest_arg_name)
                {
                    Some(ObjectFieldArgumentDefinitionPosition {
                        type_name: field_pos.type_name().clone(),
                        field_name: field_pos.field_name().clone(),
                        argument_name: dest_arg_name.clone(),
                    })
                } else {
                    None
                }
            } else {
                None
            };

            arg_sources.insert(*source_idx, arg_position);
        }

        Ok(arg_sources)
    }
    
    fn validate_field_sharing(
        &mut self,
        sources: &Sources<FieldDefinitionPosition>,
        dest: &FieldDefinitionPosition,
        merge_context: &FieldMergeContext,
    ) -> Result<(), FederationError> {
        let mut shareable_sources: Vec<SubgraphWithIndex> = Vec::new();
        let mut non_shareable_sources: Vec<SubgraphWithIndex> = Vec::new();
        let mut all_resolving: Vec<SubgraphField> = Vec::new();
        
        // Helper function to categorize a field
        let mut categorize_field = |idx: usize, subgraph: String, field: &FieldDefinitionPosition| {
            if !self.subgraphs[idx].metadata().is_field_fully_external(field) {
                all_resolving.push(SubgraphField {
                    subgraph: subgraph.clone(),
                    field: field.clone(),
                });
                if self.subgraphs[idx].metadata().is_field_shareable(field) {
                    shareable_sources.push(SubgraphWithIndex { subgraph, idx });
                } else {
                    non_shareable_sources.push(SubgraphWithIndex { subgraph, idx });
                }
            }
        };
        
        // Iterate over sources and categorize fields
        for (idx, source) in sources.iter() {
            if let Some(field) = source {
                if !merge_context.is_used_overridden(*idx) && !merge_context.is_unused_overridden(*idx) {
                    let subgraph = self.names[*idx].clone();
                    categorize_field(*idx, subgraph, field);                    
                }
            } else {
                let target: DirectiveTargetPosition = DirectiveTargetPosition::try_from(dest.clone())?;
                let itf_object_fields = self.fields_in_source_if_abstracted_by_interface_object(&target, *idx);
                for field in itf_object_fields {
                    let field_pos = FieldDefinitionPosition::try_from(field.clone())
                        .map_err(|err| FederationError::internal(err.to_string()))?;
                    let subgraph_str = format!("{} (through @interfaceObject field \"{}.{}\")", self.names[*idx], field_pos.type_name(), field_pos.field_name());
                    categorize_field(*idx, subgraph_str, &field_pos);
                }
            }
        }
        
        fn print_subgraphs<T: HasSubgraph>(arr: &Vec<T>) -> String {
            human_readable_subgraph_names(arr.iter().map(|s| s.subgraph()))
        }
        
        if !non_shareable_sources.is_empty() && (!shareable_sources.is_empty() || non_shareable_sources.len() > 1) {
            let resolving_subgraphs = print_subgraphs(&all_resolving);
            let non_shareables = if shareable_sources.is_empty() {
                "all of them".to_string()
            } else {
                print_subgraphs(&non_shareable_sources)
            };
            
            // An easy-to-make error that can lead here is the misspelling of the `from` argument of an @override. Because in that case, the
            // @override will essentially be ignored (we'll have logged a warning, but the error we're about to log will overshadow it) and
            // the 2 field instances will violate the sharing rules. But because in that case the error is ultimately with @override, it
            // can be hard for user to understand why they get a shareability error, so we detect this case and offer an additional hint
            // at what the problem might be in the error message (note that even if we do find an @override with a unknown target, we
            // cannot be 100% sure this is the issue, because this could also be targeting a subgraph that has just been removed, in which
            // case the shareable error is legit; so keep the shareability error with a strong hint is hopefully good enough in practice).
            // Note: if there is multiple non-shareable fields with "target-less overrides", we only hint about one of them, because that's
            // easier and almost surely good enough to bring the attention of the user to potential typo in @override usage.
            let subgraph_with_targetless_override = non_shareable_sources.iter().find(|s| {
                merge_context.has_override_with_unknown_target(s.idx)
            });
            
            let extra_hint = if let Some(s) = subgraph_with_targetless_override {
                format!(" (please note that \"{}.{}\" has an @override directive in {} that targets an unknown subgraph so this could be due to misspelling the @override(from:) argument)",
                    dest.type_name(),
                    dest.field_name(),
                    s.subgraph,
                )
            } else {
                "".to_string()
            };
            self.error_reporter.add_error(CompositionError::InvalidFieldSharing {
                message: format!(
                    "Non-shareable field \"{}.{}\" is resolved from multiple subgraphs: it is resolved from {} and defined as non-shareable in {}{}",
                    dest.type_name(),
                    dest.field_name(),
                    resolving_subgraphs,
                    non_shareables,
                    extra_hint,
                ),
            });
        }
        Ok(())
    }
    
}

// ============================================================================
// Join Field Directive Management
// ============================================================================

/// Properties tracked for each source index during field merging.
#[derive(Debug, Default, Clone)]
pub(crate) struct FieldMergeContextProperties {
    pub used_overridden: bool,
    pub unused_overridden: bool,
    #[allow(dead_code)]
    pub override_with_unknown_target: bool,
    pub override_label: Option<String>,
}

/// Context for field merging, holding per-source-index properties.
#[derive(Debug, Default, Clone)]
pub(crate) struct FieldMergeContext {
    props: HashMap<usize, FieldMergeContextProperties>,
}

impl FieldMergeContext {
    #[allow(dead_code)]
    pub(crate) fn new<I: IntoIterator<Item = usize>>(indices: I) -> Self {
        let mut props = HashMap::new();
        for i in indices {
            props.insert(i, FieldMergeContextProperties::default());
        }
        FieldMergeContext { props }
    }

    #[allow(dead_code)]
    pub(crate) fn is_used_overridden(&self, idx: usize) -> bool {
        self.props
            .get(&idx)
            .map(|p| p.used_overridden)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub(crate) fn is_unused_overridden(&self, idx: usize) -> bool {
        self.props
            .get(&idx)
            .map(|p| p.unused_overridden)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub(crate) fn set_used_overridden(&mut self, idx: usize) {
        if let Some(p) = self.props.get_mut(&idx) {
            p.used_overridden = true;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_unused_overridden(&mut self, idx: usize) {
        if let Some(p) = self.props.get_mut(&idx) {
            p.unused_overridden = true;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_override_with_unknown_target(&mut self, idx: usize) {
        if let Some(p) = self.props.get_mut(&idx) {
            p.override_with_unknown_target = true;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_override_label(&mut self, idx: usize, label: String) {
        if let Some(p) = self.props.get_mut(&idx) {
            p.override_label = Some(label);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn override_label(&self, idx: usize) -> Option<&str> {
        self.props
            .get(&idx)
            .and_then(|p| p.override_label.as_deref())
    }

    #[allow(dead_code)]
    pub(crate) fn has_override_with_unknown_target(&self, idx: usize) -> bool {
        self.props
            .get(&idx)
            .map(|p| p.override_with_unknown_target)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub(crate) fn some<F>(&self, mut predicate: F) -> bool
    where
        F: FnMut(&FieldMergeContextProperties, usize) -> bool,
    {
        self.props.iter().any(|(&i, p)| predicate(p, i))
    }
}

enum JoinableField<'a> {
    Output(&'a FieldDefinition),
    Input(&'a InputValueDefinition),
}

impl<'a> JoinableField<'a> {
    fn ty(&self) -> &Type {
        match self {
            JoinableField::Output(field) => &field.ty,
            JoinableField::Input(input) => &input.ty,
        }
    }

    fn directives(&self) -> &DirectiveList {
        match self {
            JoinableField::Output(field) => &field.directives,
            JoinableField::Input(input) => &input.directives,
        }
    }

    fn arguments(&self) -> Vec<Node<InputValueDefinition>> {
        match self {
            JoinableField::Output(field) => field.arguments.clone(),
            JoinableField::Input(input) => vec![Node::new((*input).clone())],
        }
    }
}

impl Merger {
    /// Adds a join__field directive to a field definition with appropriate arguments.
    /// This constructs the directive with graph, external, requires, provides, type,
    /// override, overrideLabel, usedOverridden, and contextArguments as needed.
    #[allow(dead_code)]
    pub(crate) fn add_join_field(
        &mut self,
        sources: &Sources<DirectiveTargetPosition>,
        dest: &DirectiveTargetPosition,
        all_types_equal: bool,
        merge_context: &FieldMergeContext,
    ) -> Result<(), FederationError> {
        let parent_name = match dest {
            DirectiveTargetPosition::ObjectField(pos) => &pos.type_name,
            DirectiveTargetPosition::InterfaceField(pos) => &pos.type_name,
            DirectiveTargetPosition::InputObjectField(pos) => &pos.type_name,
            _ => {
                bail!("Invalid DirectiveTargetPosition for join field: {:?}", dest);
            }
        };

        // Skip if no join__field directive is required for this field.
        match self.needs_join_field(sources, parent_name, all_types_equal, merge_context) {
            Ok(needs) if !needs => return Ok(()), // No join__field needed, exit early
            Err(_) => return Ok(()),              // Skip on error - invalid parent name
            Ok(_) => {}                           // needs join field, continue
        }

        // Filter source fields by override usage and override label presence.
        let sources_with_override = sources.iter().filter_map(|(&idx, source_opt)| {
            let used_overridden = merge_context.is_used_overridden(idx);
            let unused_overridden = merge_context.is_unused_overridden(idx);
            let override_label = merge_context.override_label(idx);

            match source_opt {
                None => None,
                Some(_) if unused_overridden && override_label.is_none() => None,
                Some(source) => Some((idx, source, used_overridden, override_label)),
            }
        });

        // Iterate through valid source fields.
        for (idx, source, used_overridden, override_label) in sources_with_override {
            // Resolve the graph enum value for this subgraph index.
            let Some(graph_name) = self.subgraph_enum_values.get(idx) else {
                continue;
            };

            let graph_value = Value::Enum(graph_name.to_name());

            let field_def = match source {
                DirectiveTargetPosition::ObjectField(pos) => {
                    let def = pos
                        .get(self.subgraphs[idx].schema().schema())
                        .map_err(|err| {
                            FederationError::internal(format!(
                                "Cannot find object field definition for subgraph {}: {}",
                                self.subgraphs[idx].name, err
                            ))
                        })?;
                    JoinableField::Output(def)
                }
                DirectiveTargetPosition::InterfaceField(pos) => {
                    let def = pos
                        .get(self.subgraphs[idx].schema().schema())
                        .map_err(|err| {
                            FederationError::internal(format!(
                                "Cannot find interface field definition for subgraph {}: {}",
                                self.subgraphs[idx].name, err
                            ))
                        })?;
                    JoinableField::Output(def)
                }
                DirectiveTargetPosition::InputObjectField(pos) => {
                    let def = pos
                        .get(self.subgraphs[idx].schema().schema())
                        .map_err(|err| {
                            FederationError::internal(format!(
                                "Cannot find input object field definition for subgraph {}: {}",
                                self.subgraphs[idx].name, err
                            ))
                        })?;
                    JoinableField::Input(def)
                }
                _ => continue,
            };

            let type_string = field_def.ty().to_string();

            let subgraph = &self.subgraphs[idx];

            let external = match source {
                DirectiveTargetPosition::ObjectField(pos) => {
                    self.is_field_external(idx, &FieldDefinitionPosition::Object(pos.clone()))
                }
                DirectiveTargetPosition::InterfaceField(pos) => {
                    self.is_field_external(idx, &FieldDefinitionPosition::Interface(pos.clone()))
                }
                _ => false, // Non-field positions can't be external
            };

            let requires = self.get_field_set(
                &field_def,
                subgraph.requires_directive_name().ok().flatten().as_ref(),
            );

            let provides = self.get_field_set(
                &field_def,
                subgraph.provides_directive_name().ok().flatten().as_ref(),
            );

            let override_from = self.get_override_from(
                &field_def,
                subgraph.override_directive_name().ok().flatten().as_ref(),
            );

            let context_arguments = self.extract_context_arguments(idx, &field_def)?;

            // Build @join__field directive with applicable arguments
            let mut builder = JoinFieldBuilder::new()
                .arg(&FEDERATION_GRAPH_ARGUMENT_NAME, graph_value)
                .maybe_bool_arg(&FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC, external)
                .maybe_arg(&FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC, requires)
                .maybe_arg(&FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC, provides)
                .maybe_arg(&FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC, override_from)
                .maybe_arg(&FEDERATION_OVERRIDE_LABEL_ARGUMENT_NAME, override_label)
                .maybe_bool_arg(&FEDERATION_USED_OVERRIDEN_ARGUMENT_NAME, used_overridden)
                .maybe_arg(&FEDERATION_CONTEXT_ARGUMENT_NAME, context_arguments.clone());

            // Include field type if not uniform across subgraphs.
            if !all_types_equal && !type_string.is_empty() {
                builder = builder.arg(&FEDERATION_TYPE_ARGUMENT_NAME, type_string);
            }

            // Attach the constructed directive to the destination field definition.
            dest.insert_directive(&mut self.merged, builder.build())?;
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn needs_join_field(
        &self,
        sources: &Sources<DirectiveTargetPosition>,
        parent_name: &Name,
        all_types_equal: bool,
        merge_context: &FieldMergeContext,
    ) -> Result<bool, FederationError> {
        // Type mismatch across subgraphs always requires join__field
        if !all_types_equal {
            return Ok(true);
        }

        // Used overrides or override labels require join__field
        if merge_context.some(|props, _| props.used_overridden || props.override_label.is_some()) {
            return Ok(true);
        }

        // Check if any field has @fromContext directive
        for source in sources.values().flatten() {
            // Check if THIS specific source is in fields_with_from_context
            match source {
                DirectiveTargetPosition::ObjectField(obj_field) => {
                    if self
                        .fields_with_from_context
                        .object_fields
                        .contains(obj_field)
                    {
                        return Ok(true);
                    }
                }
                DirectiveTargetPosition::InterfaceField(intf_field) => {
                    if self
                        .fields_with_from_context
                        .interface_fields
                        .contains(intf_field)
                    {
                        return Ok(true);
                    }
                }
                _ => continue, // Input object fields and other directive targets don't have @fromContext arguments, skip
            }
        }

        // We can avoid the join__field if:
        //   1) the field exists in all sources having the field parent type,
        //   2) none of the field instance has a @requires or @provides.
        //   3) none of the field is @external.
        for (&idx, source_opt) in sources {
            let overridden = merge_context.is_unused_overridden(idx);
            match source_opt {
                Some(source_pos) => {
                    if !overridden {
                        if let Some(subgraph) = self.subgraphs.get(idx) {
                            // Check if field is external
                            let is_external = match source_pos {
                                DirectiveTargetPosition::ObjectField(pos) => self
                                    .is_field_external(
                                        idx,
                                        &FieldDefinitionPosition::Object(pos.clone()),
                                    ),
                                DirectiveTargetPosition::InterfaceField(pos) => self
                                    .is_field_external(
                                        idx,
                                        &FieldDefinitionPosition::Interface(pos.clone()),
                                    ),
                                _ => false, // Non-field positions can't be external
                            };
                            if is_external {
                                return Ok(true);
                            }

                            // Check for requires and provides directives using subgraph-specific metadata
                            if let Ok(Some(provides_directive_name)) =
                                subgraph.provides_directive_name()
                            {
                                if !source_pos
                                    .get_applied_directives(
                                        subgraph.schema(),
                                        &provides_directive_name,
                                    )
                                    .is_empty()
                                {
                                    return Ok(true);
                                }
                            }
                            if let Ok(Some(requires_directive_name)) =
                                subgraph.requires_directive_name()
                            {
                                if !source_pos
                                    .get_applied_directives(
                                        subgraph.schema(),
                                        &requires_directive_name,
                                    )
                                    .is_empty()
                                {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
                None => {
                    // This subgraph does not have the field, so if it has the field type, we need a join__field.
                    if let Some(subgraph) = self.subgraphs.get(idx) {
                        if subgraph
                            .schema()
                            .try_get_type(parent_name.clone())
                            .is_some()
                        {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    #[allow(dead_code)]
    fn extract_context_arguments(
        &self,
        idx: usize,
        source: &JoinableField,
    ) -> Result<Option<Value>, FederationError> {
        let subgraph_name = self.subgraphs[idx].name.clone();

        // Check if the @fromContext directive is defined in the schema
        // If the directive is not defined in the schema, we cannot extract context arguments
        let Ok(Some(from_context_name)) = self.subgraphs[idx].from_context_directive_name() else {
            return Ok(None);
        };

        let directive_name = &from_context_name;

        let mut context_args: Vec<Node<Value>> = vec![];

        for arg in source.arguments().iter() {
            let Some(directive) = arg.directives.get(directive_name) else {
                continue;
            };

            let Some(field) = directive
                .specified_argument_by_name(&FEDERATION_FIELD_ARGUMENT_NAME)
                .and_then(|v| v.as_str())
            else {
                continue;
            };

            let (Some(context), Some(selection)) = parse_context(field) else {
                continue;
            };

            let prefixed_context = format!("{}__{}", subgraph_name, context);

            context_args.push(Node::new(Value::Object(vec![
                (name!("context"), Node::new(Value::String(prefixed_context))),
                (
                    FEDERATION_NAME_ARGUMENT_NAME,
                    Node::new(Value::String(arg.name.to_string())),
                ),
                (
                    FEDERATION_TYPE_ARGUMENT_NAME,
                    Node::new(Value::String(arg.ty.to_string())),
                ),
                (
                    FEDERATION_SELECTION_ARGUMENT_NAME,
                    Node::new(Value::String(selection)),
                ),
            ])));
        }

        Ok((!context_args.is_empty()).then(|| Value::List(context_args)))
    }

    /// Extract field set from directive
    #[allow(dead_code)]
    fn get_field_set(
        &self,
        field_def: &JoinableField,
        directive_name: Option<&Name>,
    ) -> Option<String> {
        let directive_name = directive_name?;

        field_def
            .directives()
            .get(directive_name)?
            .specified_argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME)?
            .as_str()
            .map(|v| v.to_string())
    }

    /// Extract override "from" argument
    #[allow(dead_code)]
    fn get_override_from(
        &self,
        field_def: &JoinableField,
        directive_name: Option<&Name>,
    ) -> Option<String> {
        let directive_name = directive_name?;

        Some(
            field_def
                .directives()
                .get(directive_name)?
                .specified_argument_by_name(&FEDERATION_FROM_ARGUMENT_NAME)?
                .as_str()?
                .to_string(),
        )
    }
}

/// Simple builder for join__field directives (minimal version for compatibility)
#[allow(dead_code)]
struct JoinFieldBuilder {
    arguments: Vec<Node<Argument>>,
}

#[allow(dead_code)]
impl JoinFieldBuilder {
    fn new() -> Self {
        Self {
            arguments: Vec::new(),
        }
    }

    fn arg<T: Into<Value>>(mut self, key: &Name, value: T) -> Self {
        self.arguments.push(Node::new(Argument {
            name: key.clone(),
            value: Node::new(value.into()),
        }));
        self
    }

    fn maybe_arg<T: Into<Value>>(self, key: &Name, value: Option<T>) -> Self {
        if let Some(v) = value {
            self.arg(key, v)
        } else {
            self
        }
    }

    fn maybe_bool_arg(self, key: &Name, condition: bool) -> Self {
        if condition {
            self.arg(key, Value::Boolean(true))
        } else {
            self
        }
    }

    fn build(self) -> Directive {
        Directive {
            name: name!("join__field"),
            arguments: self.arguments,
        }
    }
}

#[cfg(test)]
mod join_field_tests {
    use apollo_compiler::Name;
    use apollo_compiler::collections::IndexMap;

    use super::*;
    use crate::merger::merge_enum::tests::create_test_merger;
    use crate::schema::position::DirectiveTargetPosition;
    use crate::schema::position::ObjectFieldDefinitionPosition;

    // Helper function to create merge context
    fn make_merge_context(
        used_overridden: Vec<bool>,
        override_labels: Vec<Option<String>>,
    ) -> FieldMergeContext {
        let indices: Vec<usize> = (0..used_overridden.len()).collect();
        let mut context = FieldMergeContext::new(indices);

        for (idx, &used) in used_overridden.iter().enumerate() {
            if used {
                context.set_used_overridden(idx);
            }
        }

        for (idx, label) in override_labels.into_iter().enumerate() {
            if let Some(label_str) = label {
                context.set_override_label(idx, label_str);
            }
        }

        context
    }

    // Core logic tests
    #[test]
    fn test_types_differ_emits_join_field() {
        let merger = create_test_merger().expect("valid Merger object");
        let sources: Sources<DirectiveTargetPosition> = [
            (
                0,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
            (
                1,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
        ]
        .into_iter()
        .collect();
        let ctx = make_merge_context(vec![false, false], vec![None, None]);

        let result = merger.needs_join_field(&sources, &name!("Parent"), false, &ctx);
        assert!(
            result.unwrap(),
            "Should emit join field when types differ (all_types_equal = false)"
        );
    }

    #[test]
    fn test_all_types_equal_no_directives_skips() {
        let merger = create_test_merger().expect("valid Merger object");
        let sources: Sources<DirectiveTargetPosition> = [
            (
                0,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
            (
                1,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
        ]
        .into_iter()
        .collect();
        let ctx = make_merge_context(vec![false, false], vec![None, None]);

        let result = merger.needs_join_field(&sources, &name!("Parent"), true, &ctx);
        assert!(
            !result.unwrap(),
            "Should skip join field when types equal and no directives"
        );
    }

    #[test]
    fn test_needs_join_field_returns_true_when_types_differ() {
        let merger = create_test_merger().expect("valid Merger object");
        let sources: Sources<DirectiveTargetPosition> = [(
            0,
            Some(DirectiveTargetPosition::ObjectField(
                ObjectFieldDefinitionPosition {
                    type_name: Name::new("Parent").unwrap(),
                    field_name: Name::new("foo").unwrap(),
                },
            )),
        )]
        .into_iter()
        .collect();
        let ctx = make_merge_context(vec![false], vec![None]);

        // When all_types_equal = false, needs_join_field should return true
        let result = merger.needs_join_field(&sources, &name!("Parent"), false, &ctx);
        assert!(
            result.unwrap(),
            "needs_join_field should return true when types differ"
        );
    }

    #[test]
    fn test_needs_join_field_returns_false_when_not_needed() {
        let merger = create_test_merger().expect("valid Merger object");
        let sources: Sources<DirectiveTargetPosition> = [
            (
                0,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
            (
                1,
                Some(DirectiveTargetPosition::ObjectField(
                    ObjectFieldDefinitionPosition {
                        type_name: Name::new("Parent").unwrap(),
                        field_name: Name::new("foo").unwrap(),
                    },
                )),
            ),
        ]
        .into_iter()
        .collect();
        let ctx = make_merge_context(vec![false, false], vec![None, None]);

        // When all_types_equal = true and no special conditions, needs_join_field should return false
        let result = merger.needs_join_field(&sources, &name!("Parent"), true, &ctx);
        assert!(
            !result.unwrap(),
            "needs_join_field should return false when no join field is needed"
        );
    }

    #[test]
    fn test_add_join_field_early_returns_when_not_needed() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Set up a scenario where needs_join_field returns false
        let sources: Sources<DirectiveTargetPosition> = [(
            0,
            Some(DirectiveTargetPosition::ObjectField(
                ObjectFieldDefinitionPosition {
                    type_name: Name::new("Parent").unwrap(),
                    field_name: Name::new("foo").unwrap(),
                },
            )),
        )]
        .into_iter()
        .collect();
        let ctx = make_merge_context(vec![false], vec![None]);
        let dest = DirectiveTargetPosition::ObjectField(ObjectFieldDefinitionPosition {
            type_name: Name::new("Parent").unwrap(),
            field_name: Name::new("foo").unwrap(),
        });

        // Verify needs_join_field returns false first
        let needs_join = merger.needs_join_field(&sources, &name!("Parent"), true, &ctx);
        assert!(
            !needs_join.unwrap(),
            "needs_join_field should return false when all types equal and no special conditions"
        );

        // This should early return without trying to access subgraphs or subgraph_enum_values
        // because needs_join_field returns false when all_types_equal = true and no special conditions
        merger.add_join_field(&sources, &dest, true, &ctx).unwrap();

        // The test passes if no panic occurs (the early return worked)
    }

    #[test]
    fn test_field_merge_context_integration() {
        // Test that FieldMergeContext can be created and used
        let merge_context = FieldMergeContext::default();

        // Test basic methods that should be available
        let unused = merge_context.is_unused_overridden(0);
        let label = merge_context.override_label(0);

        // Assert the expected behavior
        assert!(
            !unused,
            "Default context should not have unused overridden fields"
        );
        assert!(
            label.is_none(),
            "Default context should not have override labels"
        );

        // Test that Sources works correctly with our types
        let mut sources: Sources<DirectiveTargetPosition> = IndexMap::default();
        sources.insert(0, None);

        // Test iteration
        for (idx, field_opt) in &sources {
            assert_eq!(*idx, 0);
            assert!(field_opt.is_none());
        }

        // Test that we can create a FieldMergeContext
        let context = FieldMergeContext::new(vec![0, 1]);
        assert!(
            !context.is_used_overridden(0),
            "New context should not have used overridden fields"
        );
        assert!(
            !context.is_used_overridden(1),
            "New context should not have used overridden fields"
        );
    }

    // Note: Tests for federation directive detection (like @provides, @requires, @external)
    // are not included here because they require full subgraph schemas with federation directives.
    // The create_test_merger() function creates a minimal merger with empty subgraphs,
    // so the federation directive detection logic in needs_join_field() cannot access
    // actual field definitions. These scenarios should be tested in integration tests
    // that set up complete federation schemas.

    #[test]
    fn test_field_merge_context_behavior() {
        // Test FieldMergeContext behavior that our methods rely on
        let ctx = make_merge_context(
            vec![false, true, false],
            vec![None, Some("label1".to_string()), None],
        );

        // Test override detection
        assert!(!ctx.is_used_overridden(0));
        assert!(ctx.is_used_overridden(1));
        assert!(!ctx.is_used_overridden(2));

        // Test override labels
        assert_eq!(ctx.override_label(0), None);
        assert_eq!(ctx.override_label(1), Some("label1"));
        assert_eq!(ctx.override_label(2), None);

        // Test some() method
        let has_override =
            ctx.some(|props, _idx| props.used_overridden || props.override_label.is_some());
        assert!(has_override, "Should detect override conditions");

        let no_override_ctx = make_merge_context(vec![false, false], vec![None, None]);
        let no_override = no_override_ctx
            .some(|props, _idx| props.used_overridden || props.override_label.is_some());
        assert!(
            !no_override,
            "Should not detect override conditions when none exist"
        );
    }
}
