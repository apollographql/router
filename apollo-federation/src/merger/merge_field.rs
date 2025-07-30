use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::HashSet;
use apollo_compiler::schema::ExtendedType;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge::map_sources;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::utils::human_readable::human_readable_types;

impl Merger {
    #[allow(dead_code)]
    pub(crate) fn merge_field(
        &mut self,
        sources: &Sources<FieldDefinitionPosition>,
        dest: &FieldDefinitionPosition,
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
                    .all(|f| metadata.external_metadata().is_external(f)),
                Some(s) => metadata.external_metadata().is_external(s),
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
                                human_readable_types(
                                    itf_object_fields.iter().map(|f| f.type_name().to_string())
                                )
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

        // Convert to DirectiveTargetPosition for directive merging
        let directive_sources: Sources<DirectiveTargetPosition> = without_external
            .iter()
            .map(|(&idx, source)| {
                (
                    idx,
                    source
                        .as_ref()
                        .and_then(|s| DirectiveTargetPosition::try_from(s).ok()),
                )
            })
            .collect();
        let directive_dest = DirectiveTargetPosition::try_from(dest)?;
        self.record_applied_directives_to_merge(&directive_sources, &directive_dest)?;
        self.add_arguments_shallow(&without_external, dest);
        let dest_field = dest.get(self.merged.schema())?;
        let dest_arguments = dest_field.arguments.clone();
        for dest_arg in dest_arguments.iter() {
            let subgraph_args = map_sources(&without_external, |field| {
                field.as_ref().and_then(|f| {
                    f.get(self.merged.schema())
                        .ok()?
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
                    .and_then(|pos| pos.get(self.merged.schema()).ok())
                    .map(|field_def| field_def.ty.clone());
                (*idx, type_option)
            })
            .collect();

        // Get mutable access to the dest field definition for type merging
        let dest_field_component = dest.get(self.merged.schema())?.clone();
        let mut dest_field_ast = dest_field_component.as_ref().clone();
        let dest_parent = dest.parent();
        let all_types_equal = self.merge_type_reference(
            &type_sources,
            &mut dest_field_ast,
            false,
            dest_parent.type_name(),
        )?;

        if self.has_external(sources) {
            self.validate_external_fields(sources, dest, all_types_equal)?;
        }
        self.add_join_field(sources, dest);

        // convert sources from Sources<FieldDefinitionPosition> to Sources<DirectiveTargetPosition>
        let directive_sources: Sources<DirectiveTargetPosition> = sources
            .iter()
            .map(|(&idx, source)| {
                (
                    idx,
                    source
                        .as_ref()
                        .and_then(|s| DirectiveTargetPosition::try_from(s).ok()),
                )
            })
            .collect();

        self.add_join_directive_directives(
            &directive_sources,
            DirectiveTargetPosition::try_from(dest)?,
        )?;
        Ok(())
    }

    fn fields_in_source_if_abstracted_by_interface_object(
        &self,
        dest_field: &FieldDefinitionPosition,
        source_idx: usize,
    ) -> Vec<FieldDefinitionPosition> {
        // Get the parent type of the destination field
        let parent_in_supergraph = match dest_field {
            FieldDefinitionPosition::Object(field) => {
                CompositeTypeDefinitionPosition::Object(field.parent())
            }
            FieldDefinitionPosition::Interface(field) => {
                CompositeTypeDefinitionPosition::Interface(field.parent())
            }
            FieldDefinitionPosition::Union(_) => {
                return Vec::new();
            }
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

        // Get the parent object type position (we know it's an object due to the check above)
        let parent_object = match &parent_in_supergraph {
            CompositeTypeDefinitionPosition::Object(obj) => obj,
            _ => return Vec::new(), // Should not happen due to is_object_type check above
        };

        let field_name = dest_field.field_name();

        // Get the object type from the supergraph to access its implemented interfaces
        let Ok(object_type) = parent_object.get(self.merged.schema()) else {
            return Vec::new();
        };

        // Find interface object fields that provide this field
        object_type
            .implements_interfaces
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
                        Some(FieldDefinitionPosition::Object(
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
        sources: &Sources<FieldDefinitionPosition>,
    ) -> Sources<FieldDefinitionPosition> {
        sources
            .iter()
            .fold(Sources::default(), |mut filtered, (i, source)| {
                match source {
                    // If no source or not external, mirror the input
                    None => {
                        filtered.insert(*i, source.clone());
                        filtered
                    }
                    Some(field_pos) if !self.is_field_external(*i, field_pos) => {
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

                        let _ = self.validate_external_field_directives(*i, field_pos);
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
            if self.is_merged_directive(&self.names[source_idx], &directive.name) {
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
    fn has_external(&self, sources: &Sources<FieldDefinitionPosition>) -> bool {
        Self::some_sources(sources, |source, idx| match source {
            Some(field_pos) => self.is_field_external(idx, field_pos),
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
        let mut invalid_args_presence: HashSet<Name> = Default::default();
        let mut invalid_args_types: HashSet<Name> = Default::default();
        let mut invalid_args_defaults: HashSet<Name> = Default::default();

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
}
