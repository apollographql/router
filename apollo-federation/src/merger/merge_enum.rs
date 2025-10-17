use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::link::inaccessible_spec_definition::IsInaccessibleExt;
use crate::merger::hints::HintCode;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::supergraph::CompositionHint;

#[derive(Debug, Clone)]
pub(crate) enum EnumExampleAst {
    #[allow(dead_code)]
    Field(Node<FieldDefinition>),
    #[allow(dead_code)]
    Input(Node<InputValueDefinition>),
}

#[derive(Debug, Clone)]
pub(crate) struct EnumExample {
    #[allow(dead_code)]
    pub coordinate: String,
    #[allow(dead_code)]
    pub element_ast: Option<EnumExampleAst>,
}

#[derive(Debug, Clone)]
pub(crate) enum EnumTypeUsage {
    #[allow(dead_code)]
    Input {
        input_example: EnumExample,
    },
    #[allow(dead_code)]
    Output {
        output_example: EnumExample,
    },
    #[allow(dead_code)]
    Both {
        input_example: EnumExample,
        output_example: EnumExample,
    },
    Unused,
}

impl Merger {
    /// Merge enum type from multiple subgraphs
    #[allow(dead_code)]
    pub(crate) fn merge_enum(
        &mut self,
        sources: Sources<Node<EnumType>>,
        dest: &EnumTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        let usage = self.enum_usages.get(dest.type_name.as_str()).cloned().unwrap_or_else(|| {
            // If the enum is unused, we have a choice to make. We could skip the enum entirely (after all, exposing an unreferenced type mostly "pollutes" the supergraph API), but
            // some evidence shows that many a user have such unused enums in federation 1 and having those removed from their API might be surprising. We could merge it as
            // an "input-only" or as a "input/output" type, but the hints/errors generated in both those cases would be confusing in that case, and while we could amend them
            // for this case, it would complicate things and doesn't feel like it would feel very justified. So we merge it as an "output" type, which is the least contraining
            // option. We do raise an hint though so users can notice this.
            let usage = EnumTypeUsage::Unused;
            self.error_reporter.add_hint(CompositionHint {
                code: HintCode::UnusedEnumType.code().to_string(),
                message: format!(
                    "Enum type \"{}\" is defined but unused. It will be included in the supergraph with all the values appearing in any subgraph (\"as if\" it was only used as an output type).",
                    dest.type_name
                ),
                locations: Default::default(), // PORT_NOTE: No locations in JS implementation.
            });
            usage
        });

        let mut enum_values: IndexSet<Name> = Default::default();

        enum_values.extend(
            sources
                .iter()
                .filter_map(|(_, source)| source.as_ref())
                .flat_map(|source| source.values.values())
                .map(|value| value.node.value.clone()),
        );

        // Merge each enum value
        for value_name in enum_values {
            let value_pos = dest.value(value_name);
            self.merge_enum_value(&sources, &value_pos, &usage)?;
        }

        // We could be left with an enum type with no values, and that's invalid in graphQL
        if dest.get(self.merged.schema())?.values.is_empty() {
            self.error_reporter.add_error(CompositionError::EmptyMergedEnumType {
                message: format!(
                    "None of the values of enum type \"{}\" are defined consistently in all the subgraphs defining that type. As only values common to all subgraphs are merged, this would result in an empty type.",
                    dest.type_name
                ),
                locations: self.source_locations(&sources),
            });
        }

        Ok(())
    }

    /// Merge a specific enum value across subgraphs
    /// Returns true if the value should be removed from the enum
    fn merge_enum_value(
        &mut self,
        sources: &Sources<Node<EnumType>>,
        value_pos: &EnumValueDefinitionPosition,
        usage: &EnumTypeUsage,
    ) -> Result<(), FederationError> {
        // We merge directives (and description while at it) on the value even though we might remove it later in that function,
        // but we do so because:
        // 1. this will catch any problems merging the description/directives (which feels like a good thing).
        // 2. it easier to see if the value is marked @inaccessible.

        let value_sources: Sources<&Component<EnumValueDefinition>> = sources
            .iter()
            .map(|(&idx, source)| {
                let source_value = source
                    .as_ref()
                    .and_then(|enum_type| enum_type.values.get(&value_pos.value_name));
                (idx, source_value)
            })
            .collect();

        // create new dest for the value
        let dest = Component::new(EnumValueDefinition {
            description: None,
            value: value_pos.value_name.clone(),
            directives: Default::default(),
        });
        value_pos.insert(&mut self.merged, dest)?;
        // TODO: Implement these helper methods - for now skip the actual merging
        // self.merge_description(&value_sources, &mut dest);
        // self.record_applied_directives_to_merge(&value_sources, &mut dest);
        self.add_join_enum_value(&value_sources, value_pos)?;

        let is_inaccessible = match &self.inaccessible_directive_name_in_supergraph {
            Some(name) => value_pos.is_inaccessible(&self.merged, name)?,
            None => false,
        };

        // The merging strategy depends on the enum type usage:
        //  - if it is _only_ used in position of Input type, we merge it with an "intersection" strategy (like other input types/things).
        //  - if it is _only_ used in position of Output type, we merge it with an "union" strategy (like other output types/things).
        //  - otherwise, it's used as both input and output and we can only merge it if it has the same values in all subgraphs.
        // So in particular, the value will be in the supergraph only if it is either an "output only" enum, or if the value is in all subgraphs.
        // Note that (like for input object fields), manually marking the value as @inaccessible let's use skips any check and add the value
        // regardless of inconsistencies.
        let violates_intersection_requirement = !is_inaccessible
            && sources.values().any(|source| {
                source
                    .as_ref()
                    .is_some_and(|enum_type| !enum_type.values.contains_key(&value_pos.value_name))
            });
        // We have a source (subgraph) that _has_ the enum type but not that particular enum value. If we're in the "both input and output usages",
        // that's where we have to fail. But if we're in the "only input" case, we simply don't merge that particular value and hint about it.
        match usage {
            EnumTypeUsage::Both {
                input_example,
                output_example,
            } if violates_intersection_requirement => {
                self.report_mismatch_error_with_specifics(
                    CompositionError::EnumValueMismatch {
                        message: format!(
                            "Enum type \"{}\" is used as both input type (for example, as type of \"{}\") and output type (for example, as type of \"{}\"), but value \"{}\" is not defined in all the subgraphs defining \"{}\": ",
                            &value_pos.type_name, input_example.coordinate, output_example.coordinate, &value_pos.value_name, &value_pos.type_name
                        ),
                    },
                    sources,
                    |source| {
                        source.as_ref().map_or("no", |enum_type| {
                            if enum_type.values.contains_key(&value_pos.value_name) { "yes" } else { "no" }
                        })
                    },
                );
            }
            EnumTypeUsage::Input { .. } if violates_intersection_requirement => {
                self.report_mismatch_hint(
                    HintCode::InconsistentEnumValueForInputEnum,
                    format!(
                        "Value \"{}\" of enum type \"{}\" will not be part of the supergraph as it is not defined in all the subgraphs defining \"{}\": ",
                        &value_pos.value_name, &value_pos.type_name, &value_pos.type_name
                    ),
                    sources,
                    |source| {
                        source.as_ref().is_some_and(|enum_type| {
                            enum_type.values.contains_key(&value_pos.value_name)
                        })
                    },
                );
                value_pos.remove(&mut self.merged)?;
            }
            EnumTypeUsage::Output { .. } | EnumTypeUsage::Unused => {
                self.hint_on_inconsistent_output_enum_value(
                    sources,
                    &value_pos.type_name,
                    &value_pos.value_name,
                );
            }
            _ => {
                // will get here if violates_intersection_requirement is false and usage is either Both or Input.
            }
        }
        Ok(())
    }

    fn add_join_enum_value(
        &mut self,
        sources: &Sources<&Component<EnumValueDefinition>>,
        value_pos: &EnumValueDefinitionPosition,
    ) -> Result<(), FederationError> {
        for (&idx, source) in sources.iter() {
            if source.is_some() {
                // Get the join spec name for this subgraph
                let subgraph_name = &self.names[idx];
                let Some(join_spec_name) = self.subgraph_names_to_join_spec_name.get(subgraph_name)
                else {
                    bail!(
                        "Could not find join spec name for subgraph '{}'",
                        subgraph_name
                    );
                };

                let directive = self
                    .join_spec_definition
                    .enum_value_directive(&self.merged, join_spec_name)?;
                let _ = value_pos.insert_directive(&mut self.merged, Node::new(directive));
            }
        }
        Ok(())
    }

    // TODO: These error reporting functions are not yet fully implemented
    fn hint_on_inconsistent_output_enum_value(
        &mut self,
        sources: &Sources<Node<EnumType>>,
        dest_name: &Name,
        value_name: &Name,
    ) {
        // As soon as we find a subgraph that has the type but not the member, we hint.
        for enum_type in sources.values().flatten() {
            if !enum_type.values.contains_key(value_name) {
                self.report_mismatch_hint(
                    HintCode::InconsistentEnumValueForOutputEnum,
                    format!(
                        "Value \"{value_name}\" of enum type \"{dest_name}\" has been added to the supergraph but is only defined in a subset of the subgraphs defining \"{dest_name}\": "
                    ),
                    sources,
                    |source| {
                        source.as_ref().is_some_and(|enum_type| {
                            enum_type.values.contains_key(value_name)
                        })
                    },
                );
                return;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use apollo_compiler::name;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::ComponentOrigin;
    use apollo_compiler::schema::InterfaceType;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::schema::UnionType;

    use super::*;
    use crate::JOIN_VERSIONS;
    use crate::SpecDefinition;
    use crate::error::ErrorCode;
    use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
    use crate::link::link_spec_definition::LINK_VERSIONS;
    use crate::link::spec::Version;
    use crate::merger::compose_directive_manager::ComposeDirectiveManager;
    use crate::merger::error_reporter::ErrorReporter;
    use crate::merger::merge::CompositionOptions;
    use crate::schema::FederationSchema;
    use crate::schema::position::EnumTypeDefinitionPosition;
    use crate::schema::position::PositionLookupError;

    fn insert_enum_type(schema: &mut FederationSchema, name: Name) -> Result<(), FederationError> {
        let status_pos = EnumTypeDefinitionPosition {
            type_name: name.clone(),
        };
        let dest = Node::new(EnumType {
            name: name.clone(),
            description: None,
            directives: Default::default(),
            values: Default::default(),
        });
        status_pos.pre_insert(schema)?;
        status_pos.insert(schema, dest)?;
        Ok(())
    }

    // Helper function to create a minimal merger instance for testing
    // This only initializes what's needed for merge_enum() testing
    pub(crate) fn create_test_merger() -> Result<Merger, FederationError> {
        let link_spec_definition = LINK_VERSIONS
            .find(&Version { major: 1, minor: 0 })
            .expect("LINK_VERSIONS should have version 1.0");
        let join_spec_definition = JOIN_VERSIONS
            .find(&Version { major: 0, minor: 5 })
            .expect("JOIN_VERSIONS should have version 0.5");

        let schema = Schema::builder()
            .adopt_orphan_extensions()
            .parse(
                r#"
            schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

            enum join__Graph {
                A @join__graph(name: "A", url: "http://localhost:4002/")
                B @join__graph(name: "B", url: "http://localhost:4003/")
            }

            scalar link__Import

            enum link__Purpose {
                SECURITY
                EXECUTION
            }
            "#,
                "",
            )
            .build()?;
        let mut schema = FederationSchema::new(schema)?;
        insert_enum_type(&mut schema, name!("Status"))?;
        insert_enum_type(&mut schema, name!("UnusedStatus"))?;

        // Add interface I
        let interface_pos = crate::schema::position::InterfaceTypeDefinitionPosition {
            type_name: name!("I"),
        };
        let interface_type = Node::new(InterfaceType {
            description: None,
            name: name!("I"),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        });
        interface_pos.pre_insert(&mut schema)?;
        interface_pos.insert(&mut schema, interface_type)?;

        // Add object type A implementing I
        let object_pos = crate::schema::position::ObjectTypeDefinitionPosition {
            type_name: name!("A"),
        };
        let mut object_type = ObjectType {
            description: None,
            name: name!("A"),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        };
        object_type
            .implements_interfaces
            .insert(ComponentName::from(name!("I")));
        object_pos.pre_insert(&mut schema)?;
        object_pos.insert(&mut schema, Node::new(object_type))?;

        // Add union U with member A
        let union_pos = crate::schema::position::UnionTypeDefinitionPosition {
            type_name: name!("U"),
        };
        let mut union_type = UnionType {
            description: None,
            name: name!("U"),
            directives: Default::default(),
            members: Default::default(),
        };
        union_type.members.insert(ComponentName::from(name!("A")));
        union_pos.pre_insert(&mut schema)?;
        union_pos.insert(&mut schema, Node::new(union_type))?;

        Ok(Merger {
            subgraphs: vec![],
            options: CompositionOptions::default(),
            names: vec!["subgraph1".to_string(), "subgraph2".to_string()],
            compose_directive_manager: ComposeDirectiveManager::new(),
            error_reporter: ErrorReporter::new(vec![
                "subgraph1".to_string(),
                "subgraph2".to_string(),
            ]),
            merged: schema,
            subgraph_names_to_join_spec_name: [
                (
                    "subgraph1".to_string(),
                    Name::new("SUBGRAPH1").expect("Valid name"),
                ),
                (
                    "subgraph2".to_string(),
                    Name::new("SUBGRAPH2").expect("Valid name"),
                ),
            ]
            .into_iter()
            .collect(),
            merged_federation_directive_names: Default::default(),
            merged_federation_directive_in_supergraph_by_directive_name: Default::default(),
            enum_usages: Default::default(),
            fields_with_from_context: Default::default(),
            fields_with_override: Default::default(),
            subgraph_enum_values: Vec::new(),
            inaccessible_directive_name_in_supergraph: None,
            link_spec_definition,
            join_spec_definition,
            join_directive_identities: Default::default(),
            schema_to_import_to_feature_url: Default::default(),
            latest_federation_version_used: FEDERATION_VERSIONS.latest().version().clone(),
            applied_directives_to_merge: Default::default(),
        })
    }

    // Helper function to create enum type with values
    fn create_enum_type(name: &str, values: &[&str]) -> Node<EnumType> {
        let mut enum_type = EnumType {
            name: Name::new(name).expect("Valid enum type name"),
            description: None,
            directives: Default::default(),
            values: Default::default(),
        };

        for value_name in values {
            let value_name_obj = Name::new(value_name).expect("Valid enum value name");
            let value_def = Component {
                origin: ComponentOrigin::Definition,
                node: Node::new(EnumValueDefinition {
                    description: None,
                    value: value_name_obj.clone(),
                    directives: Default::default(),
                }),
            };
            enum_type.values.insert(value_name_obj, value_def);
        }

        Node::new(enum_type)
    }

    fn get_enum_values(
        merger: &Merger,
        enum_name: &str,
    ) -> Result<Vec<String>, PositionLookupError> {
        let enum_pos = EnumTypeDefinitionPosition {
            type_name: Name::new_unchecked(enum_name),
        };
        Ok(enum_pos
            .get(merger.merged.schema())?
            .values
            .keys()
            .map(|key| key.to_string())
            .collect::<Vec<String>>())
    }

    #[test]
    fn test_merge_enum_output_only_enum_includes_all_values() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE", "PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("Status").expect("Valid enum name"),
        };

        // Set up usage as output-only (union strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Output {
                output_example: EnumExample {
                    coordinate: "field1".to_string(),
                    element_ast: None,
                },
            },
        );

        // Merge should include all values from all subgraphs for output-only enum
        let result = merger.merge_enum(sources, &dest);

        assert!(result.is_ok());
        let enum_vals =
            get_enum_values(&merger, "Status").expect("enum should exist in the supergraph");
        assert_eq!(enum_vals.len(), 3); // ACTIVE, INACTIVE, PENDING
        assert!(enum_vals.contains(&"ACTIVE".to_string()));
        assert!(enum_vals.contains(&"INACTIVE".to_string()));
        assert!(enum_vals.contains(&"PENDING".to_string()));
    }

    #[test]
    fn test_merge_enum_input_only_enum_includes_intersection() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE", "PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("Status").expect("Valid enum name"),
        };

        // Set up usage as input-only (intersection strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Input {
                input_example: EnumExample {
                    coordinate: "field1".to_string(),
                    element_ast: None,
                },
            },
        );

        // Merge should only include common values for input-only enum
        let result = merger.merge_enum(sources, &dest);

        assert!(result.is_ok());
        // Only ACTIVE should remain (intersection)
        // INACTIVE and PENDING should be removed with hints
        let enum_vals =
            get_enum_values(&merger, "Status").expect("enum should exist in the supergraph");
        assert_eq!(enum_vals.len(), 1);
        assert!(enum_vals.contains(&"ACTIVE".to_string()));
    }

    #[test]
    fn test_merge_enum_both_input_output_requires_all_values_consistent() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs with inconsistent values
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE", "PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("Status").expect("Valid enum name"),
        };

        // Set up usage as both input and output (requires consistency)
        let usage = EnumTypeUsage::Both {
            input_example: EnumExample {
                coordinate: "field1".to_string(),
                element_ast: None,
            },
            output_example: EnumExample {
                coordinate: "field2".to_string(),
                element_ast: None,
            },
        };

        merger.enum_usages.insert("Status".to_string(), usage);

        // This should generate an error for inconsistent values
        let result = merger.merge_enum(sources, &dest);

        // The function should complete but the error reporter should have errors
        assert!(result.is_ok());
        assert!(
            merger.error_reporter.has_errors(),
            "Expected errors to be reported for inconsistent enum values"
        );
    }

    #[test]
    fn test_merge_enum_empty_result_generates_error() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types that will result in empty enum after merging
        let enum1 = create_enum_type("Status", &["INACTIVE"]);
        let enum2 = create_enum_type("Status", &["PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("Status").expect("Valid enum name"),
        };

        // Set up usage as input-only (intersection strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Input {
                input_example: EnumExample {
                    coordinate: "field1".to_string(),
                    element_ast: None,
                },
            },
        );

        let result = merger.merge_enum(sources, &dest);

        assert!(result.is_ok());
        // Should be empty after merging
        let enum_vals =
            get_enum_values(&merger, "Status").expect("enum should exist in the supergraph");
        assert_eq!(enum_vals.len(), 0);

        // Error reporter should have an EmptyMergedEnumType error
        let (errors, _hints) = merger.error_reporter.into_errors_and_hints();
        assert!(errors.len() == 1);
        assert!(errors[0].code() == ErrorCode::EmptyMergedEnumType);
    }

    #[test]
    fn test_merge_enum_unused_enum_treated_as_output() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs
        let enum1 = create_enum_type("UnusedStatus", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("UnusedStatus", &["ACTIVE", "PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("UnusedStatus").expect("Valid enum name"),
        };

        // Don't set usage - this should trigger the unused enum path
        // which treats it as output-only

        let result = merger.merge_enum(sources, &dest);

        assert!(result.is_ok());
        // Should include all values (treated as output-only)
        let enum_vals =
            get_enum_values(&merger, "UnusedStatus").expect("enum should exist in the supergraph");
        assert_eq!(enum_vals.len(), 3); // ACTIVE, INACTIVE, PENDING
        assert!(enum_vals.contains(&"ACTIVE".to_string()));
        assert!(enum_vals.contains(&"INACTIVE".to_string()));
        assert!(enum_vals.contains(&"PENDING".to_string()));
        // Should generate an UnusedEnumType hint
    }

    #[test]
    fn test_merge_enum_identical_values_across_subgraphs() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create identical enum types from different subgraphs
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE", "PENDING"]);
        let enum2 = create_enum_type("Status", &["ACTIVE", "INACTIVE", "PENDING"]);

        let sources: Sources<Node<EnumType>> =
            [(0, Some(enum1)), (1, Some(enum2))].into_iter().collect();

        let dest = EnumTypeDefinitionPosition {
            type_name: Name::new("Status").expect("Valid enum name"),
        };

        // Set up usage as both input and output
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Both {
                input_example: EnumExample {
                    coordinate: "field1".to_string(),
                    element_ast: None,
                },
                output_example: EnumExample {
                    coordinate: "field2".to_string(),
                    element_ast: None,
                },
            },
        );

        let result = merger.merge_enum(sources, &dest);

        assert!(result.is_ok());
        // Should include all values since they're consistent
        let enum_vals =
            get_enum_values(&merger, "Status").expect("enum should exist in the supergraph");
        assert_eq!(enum_vals.len(), 3); // ACTIVE, INACTIVE, PENDING
        assert!(enum_vals.contains(&"ACTIVE".to_string()));
        assert!(enum_vals.contains(&"INACTIVE".to_string()));
        assert!(enum_vals.contains(&"PENDING".to_string()));
        // Should not generate any errors or hints
    }
}
