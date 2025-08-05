use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::{Component, InputObjectType};
use std::collections::HashSet;

use crate::error::{CompositionError, FederationError};
use crate::merger::hints::HintCode;
use crate::merger::merge::{Merger, Sources};
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::supergraph::CompositionHint;

impl Merger {
    #[allow(dead_code)]
    pub(crate) fn merge_input(
        &mut self,
        sources: &Sources<Node<InputObjectType>>,
        dest: &InputObjectTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        let added = self.add_fields_shallow(sources, dest);

        for (dest_field, subgraph_fields) in added {
            if let Some(field_def) = subgraph_fields.values().find_map(|f| f.as_ref()) {
                if dest_field.try_get(self.merged.schema()).is_none() {
                    dest_field.insert(&mut self.merged, field_def.clone())?;
                }
            }

            let dest_field_def = dest_field.try_get(self.merged.schema());
            let is_inaccessible = if let Some(dest_field_def) = dest_field_def {
                if let Some(inaccessible_directive) =
                    &self.inaccessible_directive_name_in_supergraph
                {
                    dest_field_def.directives.has(inaccessible_directive)
                } else {
                    false
                }
            } else {
                false
            };

            // Note: if the field is marked @inaccessible, we can always accept it to be inconsistent between subgraphs since
            // it won't be exposed in the API, and we don't hint about it because we're just doing what the user is explicitly asking.
            if !is_inaccessible
                && Self::some_sources(&subgraph_fields, |field, _idx| field.is_none())
            {
                // One of the subgraph has the input type but not that field. If the field is optional, we remove it for the supergraph
                // and issue a hint. But if it is required, we have to error out.
                let mut non_optional_subgraphs = Vec::new();
                let mut missing_subgraphs = Vec::new();

                for (idx, field) in subgraph_fields.iter() {
                    let subgraph_name = &self.names[*idx];
                    match field {
                        Some(field_def) if field_def.is_required() => {
                            non_optional_subgraphs.push(subgraph_name);
                        }
                        None => {
                            missing_subgraphs.push(subgraph_name);
                        }
                        _ => {}
                    }
                }

                if !non_optional_subgraphs.is_empty() {
                    let non_optional_subgraphs_str = non_optional_subgraphs
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let missing_subgraphs_str = missing_subgraphs
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");

                    self.error_reporter.add_error(CompositionError::InternalError {
                    message: format!(
                        "Input object field \"{}\" is required in some subgraphs but does not appear in all subgraphs: it is required in {} but does not appear in {}",
                        dest_field,
                        non_optional_subgraphs_str,
                        missing_subgraphs_str
                    ),
                });
                } else {
                    let mut present_subgraphs = Vec::new();
                    for (idx, field) in subgraph_fields.iter() {
                        if field.is_some() {
                            present_subgraphs.push(&self.names[*idx]);
                        }
                    }

                    let present_subgraphs_str = present_subgraphs
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let missing_subgraphs_str = missing_subgraphs
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");

                    self.error_reporter.add_hint(CompositionHint {
                    code: HintCode::InconsistentInputObjectField.code().to_string(),
                    message: format!(
                        "Input object field \"{}\" will not be added to \"{}\" in the supergraph as it does not appear in all subgraphs: it is defined in {} but not in {}",
                        dest_field.field_name,
                        dest.type_name,
                        present_subgraphs_str,
                        missing_subgraphs_str
                    ),
                });
                }
                // Note that we remove the element after the hint/error because we access the parent in the hint message.
                dest_field.remove(&mut self.merged)?;
            }
        }

        // We could be left with an input type with no fields, and that's invalid in GraphQL
        let final_input_object = dest.get(self.merged.schema())?;
        if final_input_object.fields.is_empty() {
            self.error_reporter.add_error(CompositionError::InternalError {
            message: format!(
                "None of the fields of input object type \"{}\" are consistently defined in all the subgraphs defining that type. As only fields common to all subgraphs are merged, this would result in an empty type.",
                dest.type_name
            ),
        });
        }

        Ok(())
    }

    fn add_fields_shallow(
        &mut self,
        sources: &Sources<Node<InputObjectType>>,
        dest: &InputObjectTypeDefinitionPosition,
    ) -> IndexMap<
        crate::schema::position::InputObjectFieldDefinitionPosition,
        Sources<Component<InputValueDefinition>>,
    > {
        let mut added: IndexMap<
            crate::schema::position::InputObjectFieldDefinitionPosition,
            Sources<Component<InputValueDefinition>>,
        > = IndexMap::default();
        let mut fields_to_add: IndexMap<usize, HashSet<Option<Component<InputValueDefinition>>>> =
            IndexMap::default();
        let mut extra_sources: Sources<Component<InputValueDefinition>> = IndexMap::default();

        // Process each subgraph
        for (source_index, source) in sources.iter() {
            let fields_set = fields_to_add
                .entry(*source_index)
                .or_insert_with(HashSet::new);

            if let Some(source_input) = source {
                // Add all fields from this subgraph
                for field in source_input.fields.values() {
                    fields_set.insert(Some(field.clone()));
                }
            }

            // Check if this subgraph defines the input type (even if source is None)
            if let Some(subgraph) = self.subgraphs.get(*source_index) {
                if subgraph.schema().get_type(dest.type_name.clone()).is_ok() {
                    extra_sources.insert(*source_index, None);
                }
            } else {
                // Primarily for tests, but we need to populate extra_sources for proper field mapping
                extra_sources.insert(*source_index, None);
            }
        }
        // Build the result map
        for (source_index, field_set) in fields_to_add {
            for field_opt in field_set {
                if let Some(field) = field_opt {
                    let dest_field_pos = dest.field(field.name.clone());

                    // Get or create the sources map for this destination field
                    let field_sources = added.entry(dest_field_pos.clone()).or_insert_with(|| {
                        // Start with extra_sources (subgraphs that define the type but may not have this field)
                        extra_sources.clone()
                    });

                    // Set the actual field for this subgraph
                    field_sources.insert(source_index, Some(field));
                }
            }
        }

        added
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::ast::{InputValueDefinition, Type};
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::schema::{Component, InputObjectType};

    use super::*;
    use crate::merger::merge_enum::tests::create_test_merger;
    use crate::schema::position::InputObjectTypeDefinitionPosition;

    // Helper function to create an input object type for testing
    fn create_input_object_type(
        name: &str,
        field_definitions: &[(&str, &str, bool)],
    ) -> Node<InputObjectType> {
        let mut input_object_type = InputObjectType {
            description: None,
            name: Name::new_unchecked(name),
            directives: Default::default(),
            fields: IndexMap::default(),
        };

        for (field_name, field_type, is_required) in field_definitions {
            let field_type = if *is_required {
                Type::NonNullNamed(Name::new_unchecked(field_type))
            } else {
                Type::Named(Name::new_unchecked(field_type))
            };

            let field_def = InputValueDefinition {
                description: None,
                name: Name::new_unchecked(field_name),
                ty: field_type.into(),
                default_value: None,
                directives: Default::default(),
            };

            input_object_type
                .fields
                .insert(Name::new_unchecked(field_name), Component::new(field_def));
        }

        Node::new(input_object_type)
    }

    fn insert_input_object_type(merger: &mut Merger, name: &str) -> Result<(), FederationError> {
        let input_pos = InputObjectTypeDefinitionPosition {
            type_name: Name::new(name).expect("Valid name"),
        };
        let input_type = create_input_object_type(name, &[]);
        input_pos.pre_insert(&mut merger.merged)?;
        input_pos.insert(&mut merger.merged, input_type)?;
        Ok(())
    }

    // Helper function to create InputObjectTypeDefinitionPosition for testing
    fn create_input_position(name: &str) -> InputObjectTypeDefinitionPosition {
        InputObjectTypeDefinitionPosition {
            type_name: Name::new(name).expect("Valid name"),
        }
    }

    #[test]
    fn test_input_object_type_creation() {
        let input1 = create_input_object_type(
            "UserInput",
            &[("name", "String", true), ("email", "String", false)],
        );
        assert_eq!(input1.fields.len(), 2);
        assert!(
            input1
                .fields
                .contains_key(&Name::new("name").expect("Valid name"))
        );
        assert!(
            input1
                .fields
                .contains_key(&Name::new("email").expect("Valid name"))
        );

        // Check that name field is required (NonNull)
        let name_field = input1
            .fields
            .get(&Name::new("name").expect("Valid name"))
            .unwrap();
        assert!(name_field.ty.is_non_null());

        // Check that email field is optional (nullable)
        let email_field = input1
            .fields
            .get(&Name::new("email").expect("Valid name"))
            .unwrap();
        assert!(!email_field.ty.is_non_null());
    }

    #[test]
    fn test_merge_input_combines_all_fields() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects with different fields
        let input1 = create_input_object_type(
            "UserInput",
            &[("name", "String", true), ("email", "String", false)],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[("name", "String", true), ("age", "Int", false)],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");

        let result = merger.merge_input(&sources, &dest);
        assert!(result.is_ok());

        // Should contain all unique fields from both sources that appear in all subgraphs
        let final_input = dest
            .get(merger.merged.schema())
            .expect("input in supergraph");

        // Only 'name' should remain as it's present in both subgraphs
        assert_eq!(final_input.fields.len(), 1);
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("name").expect("Valid name"))
        );
    }

    #[test]
    fn test_merge_input_identical_fields_across_subgraphs() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects with identical fields
        let input1 = create_input_object_type(
            "UserInput",
            &[("name", "String", true), ("email", "String", false)],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[("name", "String", true), ("email", "String", false)],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");

        let result = merger.merge_input(&sources, &dest);
        assert!(result.is_ok());

        let final_input = dest
            .get(merger.merged.schema())
            .expect("input in supergraph");

        // Should contain both fields
        assert_eq!(final_input.fields.len(), 2);
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("name").expect("Valid name"))
        );
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("email").expect("Valid name"))
        );

        // Verify that no hints were generated
        let (_errors, hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(hints.len(), 0);
    }

    #[test]
    fn test_hint_on_inconsistent_optional_input_field() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects where one subgraph is missing an optional field
        let input1 = create_input_object_type(
            "UserInput",
            &[
                ("name", "String", true),
                ("email", "String", false), // Optional field
            ],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[
                ("name", "String", true), // Missing email field
            ],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");
        let result = merger.merge_input(&sources, &dest);

        assert!(result.is_ok());

        // Verify that hint was generated
        let (_errors, hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(hints.len(), 1);
        assert!(
            hints[0]
                .code
                .contains(HintCode::InconsistentInputObjectField.code())
        );
        assert!(hints[0].message.contains("email"));
        assert!(hints[0].message.contains("UserInput"));

        // Email field should be removed, only name should remain
        let final_input = dest
            .get(merger.merged.schema())
            .expect("input in supergraph");
        assert_eq!(final_input.fields.len(), 1);
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("name").expect("Valid name"))
        );
        assert!(
            !final_input
                .fields
                .contains_key(&Name::new("email").expect("Valid name"))
        );
    }

    #[test]
    fn test_error_on_required_field_missing_in_subgraph() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects where one subgraph is missing a required field
        let input1 = create_input_object_type(
            "UserInput",
            &[
                ("name", "String", true), // Required field
                ("email", "String", false),
            ],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[
                ("email", "String", false), // Missing required name field
            ],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");
        let result = merger.merge_input(&sources, &dest);

        assert!(result.is_ok());

        // Verify that error was generated for required field mismatch
        let (errors, _hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].to_string().contains("required in some subgraphs"));
        assert!(errors[0].to_string().contains("name"));
    }

    #[test]
    fn test_error_on_empty_input_type() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects with completely different optional fields
        let input1 = create_input_object_type(
            "UserInput",
            &[
                ("field1", "String", false), // Optional field only in subgraph 1
            ],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[
                ("field2", "String", false), // Optional field only in subgraph 2
            ],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");
        let result = merger.merge_input(&sources, &dest);

        assert!(result.is_ok());

        // Verify that error was generated for empty input type
        let (errors, hints) = merger.error_reporter.into_errors_and_hints();

        // Should have hints for inconsistent fields and error for empty type
        assert_eq!(hints.len(), 2); // One hint for each removed field
        assert_eq!(errors.len(), 1); // One error for empty type
        assert!(errors[0].to_string().contains("empty type"));
        assert!(errors[0].to_string().contains("UserInput"));
    }

    #[test]
    fn test_merge_input_with_common_and_unique_fields() {
        let mut merger = create_test_merger().expect("Valid merger");

        // Create input object in supergraph
        insert_input_object_type(&mut merger, "UserInput").expect("added UserInput to supergraph");

        // Create input objects with some common and some unique fields
        let input1 = create_input_object_type(
            "UserInput",
            &[
                ("name", "String", true),   // Common required field
                ("email", "String", false), // Common optional field
                ("phone", "String", false), // Unique to subgraph 1
            ],
        );
        let input2 = create_input_object_type(
            "UserInput",
            &[
                ("name", "String", true),     // Common required field
                ("email", "String", false),   // Common optional field
                ("address", "String", false), // Unique to subgraph 2
            ],
        );

        let sources: Sources<Node<InputObjectType>> =
            [(0, Some(input1)), (1, Some(input2))].into_iter().collect();

        let dest = create_input_position("UserInput");
        let result = merger.merge_input(&sources, &dest);

        assert!(result.is_ok());

        // Should only keep common fields (name, email)
        let final_input = dest
            .get(merger.merged.schema())
            .expect("input in supergraph");
        assert_eq!(final_input.fields.len(), 2);
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("name").expect("Valid name"))
        );
        assert!(
            final_input
                .fields
                .contains_key(&Name::new("email").expect("Valid name"))
        );

        // Verify hints were generated for removed fields
        let (_errors, hints) = merger.error_reporter.into_errors_and_hints();
        assert_eq!(hints.len(), 2); // One for phone, one for address
    }
}
