use indexmap::IndexSet;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::utils::human_readable::human_readable_types;

impl Merger {
    // Returns whether the interface has a key (even a non-resolvable one) in any subgraph.
    fn validate_interface_keys(
        &mut self,
        sources: &Sources<Subgraph<Validated>>,
        dest: &InterfaceTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // Remark: it might be ok to filter @inaccessible types in `supergraphImplementations`, but this requires
        // some more thinking (and I'm not even sure it makes a practical difference given the rules for validity
        // of @inaccessible) and it will be backward compatible to filter them later, while the reverse wouldn't
        // technically be, so we stay on the safe side.
        let supergraph_implementations = self.merged.possible_runtime_types(dest.clone().into())?;

        // Validate that if a source defines a (resolvable) @key on an interface, then that subgraph defines
        // all the implementations of that interface in the supergraph.
        let mut has_key = false;
        for (idx, source) in sources.iter() {
            let Some(subgraph) = source else {
                continue;
            };
            if !subgraph.schema().is_interface(&dest.type_name) {
                continue;
            }

            let source_metadata = self.subgraphs[*idx].metadata();
            let key_directive_name = source_metadata
                .federation_spec_definition()
                .key_directive_definition(&self.merged)?
                .name
                .clone();
            let interface_pos: TypeDefinitionPosition = dest.clone().into();
            let keys = interface_pos.get_applied_directives(subgraph.schema(), &key_directive_name);
            has_key = has_key || !keys.is_empty();
            let Some(resolvable_key) = keys.iter().find(|key| !key.arguments.is_empty()) else {
                continue;
            };
            let implementations_in_subgraph = subgraph
                .schema()
                .possible_runtime_types(dest.clone().into())?;
            if implementations_in_subgraph.len() < supergraph_implementations.len() {
                let missing_implementations = supergraph_implementations
                    .iter()
                    .filter(|implementation| !implementations_in_subgraph.contains(*implementation))
                    .collect::<IndexSet<_>>();

                self.error_reporter.add_error(CompositionError::InterfaceKeyMissingImplementationType {
                            message: format!("Interface type \"{}\" has a resolvable key \"{}\" in subgraph \"{}\" but is missing some of the supergraph implementation types of \"{}\". Subgraph \"{}\" should define {} (and have {} implement \"{}\").",
                                &dest.type_name,
                                resolvable_key.serialize(),
                                &self.subgraphs[*idx].name,
                                &dest.type_name,
                                &self.subgraphs[*idx].name,
                                human_readable_types(missing_implementations.iter().map(|impl_type| &impl_type.type_name)),
                                if missing_implementations.len() > 1 { "them" } else { "it" },
                                &dest.type_name,
                            )
                        });
            }
        }
        Ok(has_key)
    }

    fn validate_interface_objects(
        &mut self,
        sources: &Sources<Subgraph<Validated>>,
        dest: &InterfaceTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        let supergraph_implementations = self.merged.possible_runtime_types(dest.clone().into())?;

        // Validates that if a source defines the interface as an @interfaceObject, then it doesn't define any
        // of the implementations. We can discuss if there is ways to lift that limitation later, but an
        // @interfaceObject already "provides" fields for all the underlying impelmentations, so also defining
        // one those implementation would require additional care for shareability and more. This also feel
        // like this can get easily be done by mistake and gets rather confusing, so it's worth some additional
        // consideration before allowing.
        for (idx, source) in sources.iter() {
            let schema = if let Some(subgraph) = source {
                subgraph.schema().schema()
            } else {
                continue;
            };
            if !self.subgraphs[*idx].is_interface_object_type(&dest.clone().into()) {
                continue;
            }

            let subgraph_name = &self.subgraphs[*idx].name;

            let defined_implementations: IndexSet<_> = supergraph_implementations
                .iter()
                .filter(|implementation| implementation.get(schema).is_ok())
                .collect::<IndexSet<_>>();
            if !defined_implementations.is_empty() {
                self.error_reporter.add_error(CompositionError::InterfaceObjectUsageError {
                    message: format!("Interface type \"{}\" is defined as an @interfaceObject in subgraph \"{}\" so that subgraph should not define any of the implementation types of \"{}\", but it defines {}",
                        &dest.type_name,
                        &subgraph_name,
                        &dest.type_name,
                        human_readable_types(defined_implementations.iter().map(|impl_type| &impl_type.type_name)),
                    )
                });
            }
        }

        Ok(())
    }

    pub(crate) fn merge_interface(
        &mut self,
        itf: InterfaceTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        let has_key = self.validate_interface_keys(&self.subgraph_sources(), &itf)?;
        self.validate_interface_objects(&self.subgraph_sources(), &itf)?;

        let added = self.add_fields_shallow(itf.clone())?;

        for (dest_field, subgraph_fields) in added {
            if !has_key {
                self.hint_on_inconsistent_value_type_field(
                    &self.subgraph_sources(),
                    &ObjectOrInterfaceTypeDefinitionPosition::Interface(itf.clone()),
                    &dest_field,
                )?;
            }
            let merge_context = self.validate_override(&subgraph_fields, &dest_field)?;
            self.merge_field(&subgraph_fields, &dest_field, &merge_context)?;
        }
        Ok(())
    }
}
