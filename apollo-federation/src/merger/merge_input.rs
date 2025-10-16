use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::InputObjectType;
use tracing::instrument;
use tracing::trace;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SubgraphLocation;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::merger::merge_field::FieldMergeContext;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::utils::human_readable::human_readable_subgraph_names;

impl Merger {
    #[instrument(skip(self, sources, dest))]
    pub(crate) fn merge_input(
        &mut self,
        sources: &Sources<Node<InputObjectType>>,
        dest: &InputObjectTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        // Like for other inputs, we add all the fields found in any subgraphs initially as a simple mean to have a complete list of
        // field to iterate over, but we will remove those that are not in all subgraphs.
        let added = self.add_input_fields_shallow(sources, dest)?;
        trace!("Shallow added input fields: {:#?}", added.values());

        for (dest_field, subgraph_fields) in added {
            // We merge the details of the field first, even if we may remove it afterwards because 1) this ensure we always checks type
            // compatibility between definitions and 2) we actually want to see if the result is marked inaccessible or not and it makes
            // that easier.
            if let Err(e) = self.merge_input_field(&dest_field, &subgraph_fields) {
                self.error_reporter
                    .add_error(CompositionError::InputFieldMergeFailed {
                        message: format!(
                            "Failed to merge input field \"{}.{}\": {}",
                            dest_field.type_name, dest_field.field_name, e
                        ),
                        locations: self.source_locations(sources),
                    });
                continue;
            }

            let is_inaccessible = self
                .inaccessible_directive_name_in_supergraph
                .as_ref()
                .map(|directive_name| {
                    dest_field.has_applied_directive(&self.merged, directive_name)
                })
                .unwrap_or(false);

            // Note: if the field is marked @inaccessible, we can always accept it to be inconsistent between subgraphs since
            // it won't be exposed in the API, and we don't hint about it because we're just doing what the user is explicitly asking.
            if !is_inaccessible
                && Self::some_sources(&subgraph_fields, |field, _idx| field.is_none())
            {
                // One of the subgraph has the input type but not that field. If the field is optional, we remove it for the supergraph
                // and issue a hint. But if it is required, we have to error out.
                let mut non_optional_subgraphs: Vec<String> = Vec::new();
                let mut missing_subgraphs: Vec<String> = Vec::new();

                for (idx, field) in subgraph_fields.iter() {
                    let subgraph_name = &self.names[*idx];

                    match field {
                        Some(field_def) if field_def.get(self.merged.schema())?.is_required() => {
                            non_optional_subgraphs.push(subgraph_name.to_string());
                        }
                        None => {
                            missing_subgraphs.push(subgraph_name.to_string());
                        }
                        _ => {}
                    }
                }

                if !non_optional_subgraphs.is_empty() {
                    let non_optional_subgraphs_str =
                        human_readable_subgraph_names(non_optional_subgraphs.iter());
                    let missing_subgraphs_str =
                        human_readable_subgraph_names(missing_subgraphs.iter());

                    self.error_reporter.add_error(CompositionError::RequiredInputFieldMissingInSomeSubgraph {
                        message: format!(
                            "Input object field \"{}\" is required in some subgraphs but does not appear in all subgraphs: it is required in {} but does not appear in {}",
                            dest_field,
                            non_optional_subgraphs_str,
                            missing_subgraphs_str
                        ),
                        locations: self.source_locations(sources),
                    });
                } else {
                    let mut present_subgraphs = Vec::new();
                    let mut locations = Vec::new();

                    // Extract nodes and create locations for fields that exist
                    for (idx, field) in subgraph_fields.iter() {
                        if let Some(field_component) = field {
                            let _ = self
                                .names
                                .get(*idx)
                                .map(|n| present_subgraphs.push(n.to_string()));
                            if let Some(subgraph) = self.subgraphs.get(*idx) {
                                let field_node = Node::new(field_component);
                                let field_locations = subgraph
                                    .schema()
                                    .node_locations(&field_node)
                                    .map(|loc| SubgraphLocation {
                                        subgraph: subgraph.name.clone(),
                                        range: loc,
                                    });
                                locations.extend(field_locations);
                            }
                        }
                    }

                    self.error_reporter.report_mismatch_hint::<InputObjectFieldDefinitionPosition, InputObjectFieldDefinitionPosition, ()>(
                            HintCode::InconsistentDescription,
                            format!("Input object field \"{}\" will not be added to \"{}\" in the supergraph as it does not appear in all subgraphs: it is ",
                                dest_field.field_name, dest.type_name
                            ),
                            &dest_field,
                            &subgraph_fields,
                            |_| Some("yes".to_string()),
                            |_, _| Some("yes".to_string()),
                            |_, subgraphs| {
                                format!(
                                    "it is defined in {}", subgraphs.unwrap_or_else(|| "undefined".to_string())
                                )
                            },
                            |_, subgraphs| {
                                format!(
                                    "but not in {}",
                                    subgraphs,
                                )
                            },
                            true,
                            false,
                        );
                }
                // Note that we remove the element after the hint/error because we access the parent in the hint message.
                dest_field.remove(&mut self.merged)?;
            }
        }

        // We could be left with an input type with no fields, and that's invalid in GraphQL
        let final_input_object = dest.get(self.merged.schema())?;
        if final_input_object.fields.is_empty() {
            let locations = self.source_locations(sources);
            self.error_reporter.add_error(CompositionError::EmptyMergedInputType {
                message: format!(
                    "None of the fields of input object type \"{}\" are consistently defined in all the subgraphs defining that type. As only fields common to all subgraphs are merged, this would result in an empty type.",
                    dest.type_name
                ),
                locations,
            });
        }

        Ok(())
    }

    /// Adds a shallow copy of each field in an InputObject type to the supergraph schema. This is
    /// an implementation of `addFieldsShallow` from the JS implementation, but specialized to
    /// InputObject fields. As such, any logic specific to Object and Interface types is removed in
    /// this implementation. See [Merger::add_fields_shallow] for the equivalent implementation for
    /// those types.
    fn add_input_fields_shallow(
        &mut self,
        sources: &Sources<Node<InputObjectType>>,
        dest: &InputObjectTypeDefinitionPosition,
    ) -> Result<
        IndexMap<InputObjectFieldDefinitionPosition, Sources<InputObjectFieldDefinitionPosition>>,
        FederationError,
    > {
        let mut added: IndexMap<
            InputObjectFieldDefinitionPosition,
            Sources<InputObjectFieldDefinitionPosition>,
        > = Default::default();
        let mut fields_to_add: HashMap<usize, HashSet<InputObjectFieldDefinitionPosition>> =
            Default::default();
        let mut field_types: HashMap<InputObjectFieldDefinitionPosition, Node<Type>> =
            Default::default();
        let mut extra_sources: Sources<InputObjectFieldDefinitionPosition> = Default::default();

        for (idx, source) in sources {
            if let Some(source) = source {
                for field in source.fields.values() {
                    let pos = InputObjectFieldDefinitionPosition {
                        type_name: dest.type_name.clone(),
                        field_name: field.name.clone(),
                    };
                    fields_to_add.entry(*idx).or_default().insert(pos.clone());
                    field_types.insert(pos, field.ty.clone());
                }
            }

            if self.subgraphs[*idx]
                .schema()
                .try_get_type(dest.type_name.clone())
                .is_some()
            {
                // Our needsJoinField logic adds @join__field if any subgraphs define
                // the parent type containing the field but not the field itself. In
                // those cases, for each field we add, we need to add undefined entries
                // for each subgraph that defines the parent object/interface/input
                // type. We do this by populating extraSources with undefined entries
                // here, then create each new Sources map from that starting set (see
                // `new Map(extraSources)` below).
                extra_sources.insert(*idx, None);
            }
        }

        for (idx, field_set) in fields_to_add {
            for field in field_set {
                // While the JS implementation checked `isMergedField` here, that would always
                // return true for input fields, so we omit that check.
                if !added.contains_key(&field)
                    && let Some(ty) = field_types.get(&field)
                {
                    field.insert(
                        &mut self.merged,
                        Component::new(InputValueDefinition {
                            description: None,
                            name: field.field_name.clone(),
                            default_value: None,
                            ty: ty.clone(),
                            directives: Default::default(),
                        }),
                    )?;
                }
                added
                    .entry(field.clone())
                    .or_insert_with(|| extra_sources.clone())
                    .insert(idx, Some(field));
            }
        }

        Ok(added)
    }

    #[instrument(skip(self, dest_field, sources))]
    fn merge_input_field(
        &mut self,
        dest_field: &InputObjectFieldDefinitionPosition,
        sources: &Sources<InputObjectFieldDefinitionPosition>,
    ) -> Result<(), FederationError> {
        trace!("Deep merging input field: {:#?}", dest_field);
        self.merge_description(sources, dest_field)?;
        self.record_applied_directives_to_merge(sources, dest_field)?;
        let all_types_equal = self.merge_type_reference(sources, dest_field, true)?;
        let directive_sources: Sources<DirectiveTargetPosition> = sources
            .iter()
            .map(|(&idx, source_opt)| {
                let directive_pos = source_opt
                    .clone()
                    .map(DirectiveTargetPosition::InputObjectField);
                (idx, directive_pos)
            })
            .collect();

        let merge_context = FieldMergeContext::new(sources.keys().copied());
        let dest_directive = DirectiveTargetPosition::InputObjectField(dest_field.clone());
        self.add_join_field(
            &directive_sources,
            &dest_directive,
            all_types_equal,
            &merge_context,
        )?;

        self.merge_default_value(sources, dest_field)?;
        Ok(())
    }
}
