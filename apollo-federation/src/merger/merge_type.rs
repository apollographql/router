use apollo_compiler::Name;
use apollo_compiler::schema::Component;
use tracing::instrument;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::HasLocations;
use crate::link::federation_spec_definition::FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_RESOLVABLE_ARGUMENT_NAME;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::SchemaElement;
use crate::schema::position::TypeDefinitionPosition;

impl Merger {
    #[instrument(skip(self))]
    pub(in crate::merger) fn merge_type(&mut self, type_def: &Name) -> Result<(), FederationError> {
        let Ok(dest) = self.merged.get_type(type_def.clone()) else {
            bail!(
                "Type \"{}\" is missing, but it should have been shallow-copied to the supergraph schema",
                type_def
            );
        };
        let sources: Sources<TypeDefinitionPosition> = self
            .subgraphs
            .iter()
            .enumerate()
            .map(|(idx, subgraph)| (idx, subgraph.schema().get_type(type_def.clone()).ok()))
            .collect();

        self.check_for_extension_with_no_base(&sources, &dest);
        self.merge_description(&sources, &dest)?;
        self.add_join_type(&sources, &dest)?;
        self.record_applied_directives_to_merge(&sources, &dest)?;
        self.add_join_directive_directives(&sources, &dest)?;
        match dest {
            TypeDefinitionPosition::Scalar(_) => {
                // Since we don't handle applied directives yet, we have nothing specific to do for scalars.
            }
            TypeDefinitionPosition::Object(obj) => {
                self.merge_object(obj)?;
            }
            TypeDefinitionPosition::Interface(itf) => {
                self.merge_interface(itf)?;
            }
            TypeDefinitionPosition::Union(un) => {
                let sources = sources
                    .iter()
                    .map(|(idx, pos)| {
                        if let Some(TypeDefinitionPosition::Union(p)) = pos {
                            let schema = self.subgraphs[*idx].schema().schema();
                            (*idx, p.get(schema).ok().cloned())
                        } else {
                            (*idx, None)
                        }
                    })
                    .collect();
                self.merge_union(sources, &un)?;
            }
            TypeDefinitionPosition::Enum(en) => {
                let sources = sources
                    .iter()
                    .map(|(idx, pos)| {
                        if let Some(TypeDefinitionPosition::Enum(p)) = pos {
                            let schema = self.subgraphs[*idx].schema().schema();
                            (*idx, p.get(schema).ok().cloned())
                        } else {
                            (*idx, None)
                        }
                    })
                    .collect();
                self.merge_enum(sources, &en)?;
            }
            TypeDefinitionPosition::InputObject(io) => {
                let sources = sources
                    .iter()
                    .map(|(idx, pos)| {
                        if let Some(TypeDefinitionPosition::InputObject(p)) = pos {
                            let schema = self.subgraphs[*idx].schema().schema();
                            (*idx, p.get(schema).ok().cloned())
                        } else {
                            (*idx, None)
                        }
                    })
                    .collect();
                self.merge_input(&sources, &io)?;
            }
        }

        Ok(())
    }

    /// Records errors if this type is defined as an extension in all subgraphs where it is
    /// present. This is a good candidate to move to the pre-merge validations phase, but we
    /// currently check during merge because it requires visiting all types in the schema.
    fn check_for_extension_with_no_base(
        &mut self,
        sources: &Sources<TypeDefinitionPosition>,
        dest: &TypeDefinitionPosition,
    ) {
        if dest.is_object_type() && self.merged.is_root_type(dest.type_name()) {
            return;
        }

        let mut subgraphs_with_extension = Vec::with_capacity(sources.len());
        for (idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };
            let subgraph = &self.subgraphs[*idx];
            let schema = subgraph.schema().schema();
            let Ok(element) = source.get(schema) else {
                continue;
            };

            if element.has_non_extension_elements() {
                // Found a definition => no error.
                return;
            }

            if element.has_extension_elements() {
                let subgraph_name = subgraph.name.to_string();
                let element_locations = element.locations(subgraph);
                subgraphs_with_extension.push((subgraph_name, element_locations));
            }
        }

        for (subgraph, locations) in subgraphs_with_extension {
            self.error_reporter_mut()
                .add_error(CompositionError::ExtensionWithNoBase {
                    subgraph,
                    dest: dest.type_name().to_string(),
                    locations,
                });
        }
    }

    /// Applies a `@join__type` directive to the merged schema for each `@key` in the source
    /// schema. If this type has no `@key` applications, a single `@join__type` is applied for this
    /// type.
    fn add_join_type(
        &mut self,
        sources: &Sources<TypeDefinitionPosition>,
        dest: &TypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        for (idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };

            let subgraph = &self.subgraphs[*idx];
            let is_interface_object = subgraph.is_interface_object_type(source);

            let key_directive_name = subgraph.key_directive_name()?;
            let keys = if let Some(key_directive_name) = &key_directive_name {
                source.get_applied_directives(subgraph.schema(), key_directive_name)
            } else {
                Vec::new()
            };

            let name = self.join_spec_name(*idx)?.clone();

            if keys.is_empty() {
                // If there are no keys, we apply a single `@join__type` for this type.
                let directive = self.join_spec_definition.type_directive(
                    &self.merged,
                    name,
                    None,
                    None,
                    None,
                    is_interface_object.then_some(is_interface_object),
                )?;
                dest.insert_directive(&mut self.merged, Component::new(directive))?;
            } else {
                // If this type has keys, we apply a `@join__type` for each key.
                let extends_directive_name = subgraph
                    .extends_directive_name()
                    .ok()
                    .flatten()
                    .clone()
                    .unwrap_or(FEDERATION_EXTENDS_DIRECTIVE_NAME_IN_SPEC);

                for key in keys {
                    let extension = key.origin.extension_id().is_some()
                        || source.has_applied_directive(subgraph.schema(), &extends_directive_name);
                    let key_fields =
                        key.specified_argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME);
                    let key_resolvable =
                        key.specified_argument_by_name(&FEDERATION_RESOLVABLE_ARGUMENT_NAME);

                    // Since the boolean values for this directive default to `false`, we return
                    // `None` instead of `Some(false)` so the value can be omitted from the
                    // supergraph schema. This generates smaller schemas and makes it easier to
                    // check equality between schemas which may not support some of the arguments.
                    let directive = self.join_spec_definition.type_directive(
                        &self.merged,
                        name.clone(),
                        key_fields.cloned(),
                        extension.then_some(extension),
                        key_resolvable.and_then(|v| v.to_bool()),
                        is_interface_object.then_some(is_interface_object),
                    )?;
                    dest.insert_directive(&mut self.merged, Component::new(directive))?;
                }
            }
        }

        Ok(())
    }
}
