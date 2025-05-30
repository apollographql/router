use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::validation::Valid;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::inaccessible_spec_definition::IsInaccessibleExt;
use crate::link::join_spec_definition::JOIN_VERSIONS;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::schema::FederationSchema;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

/// Type alias for Sources mapping - maps subgraph indices to optional values
type Sources<T> = IndexMap<usize, Option<T>>;

#[derive(Debug, Clone)]
enum EnumTypeUsage {
    #[allow(dead_code)]
    Input {
        input_example: String,
    },
    #[allow(dead_code)]
    Output {
        output_example: String,
    },
    #[allow(dead_code)]
    Both {
        input_example: String,
        output_example: String,
    },
    Unused,
}

#[derive(Debug)]
pub(crate) struct MergeResult {
    #[allow(dead_code)]
    pub(crate) supergraph: Option<Valid<FederationSchema>>,
    #[allow(dead_code)]
    pub(crate) errors: Vec<SingleFederationError>,
    #[allow(dead_code)]
    pub(crate) hints: Vec<CompositionHint>,
}

#[derive(Debug, Default)]
pub(crate) struct CompositionOptions {
    // Add options as needed - for now keeping it minimal
}

#[allow(unused)]
pub(crate) struct Merger {
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
    names: Vec<String>,
    error_reporter: ErrorReporter,
    merged: FederationSchema,
    subgraph_names_to_join_spec_name: HashMap<String, String>,
    merged_federation_directive_names: HashSet<String>,
    enum_usages: HashMap<String, EnumTypeUsage>,
    fields_with_from_context: HashSet<String>,
    fields_with_override: HashSet<String>,
    inaccessible_directive_name_in_supergraph: Option<Name>,
    join_spec_definition: &'static JoinSpecDefinition,
}

#[allow(unused)]
impl Merger {
    pub(crate) fn new(
        subgraphs: Vec<Subgraph<Validated>>,
        options: CompositionOptions,
    ) -> Result<Self, FederationError> {
        let names: Vec<String> = subgraphs.iter().map(|s| s.name.clone()).collect();

        // TODO: In the future, get this from getLatestFederationVersionUsed() instead of using latest
        let join_spec_definition = JOIN_VERSIONS
            .find(&crate::link::spec::Version { major: 0, minor: 5 })
            .expect("JOIN_VERSIONS should have version 0.5");

        Ok(Self {
            subgraphs,
            options,
            names,
            error_reporter: ErrorReporter::new(),
            merged: FederationSchema::new(Schema::new())?,
            subgraph_names_to_join_spec_name: todo!(),
            merged_federation_directive_names: todo!(),
            enum_usages: HashMap::new(),
            fields_with_from_context: todo!(),
            fields_with_override: todo!(),
            inaccessible_directive_name_in_supergraph: todo!(),
            join_spec_definition,
        })
    }

    pub(crate) fn merge(mut self) -> MergeResult {
        // Validate compose directive manager
        self.validate_compose_directive_manager();

        // Add core features to the merged schema
        self.add_core_features();

        // Create empty objects for all types and directive definitions
        self.add_types_shallow();
        self.add_directives_shallow();

        // Collect types by category
        let mut object_types: Vec<Name> = Vec::new();
        let mut interface_types: Vec<Name> = Vec::new();
        let mut union_types: Vec<Name> = Vec::new();
        let mut enum_types: Vec<Name> = Vec::new();
        let mut non_union_enum_types: Vec<Name> = Vec::new();

        // TODO: Iterate through merged.types() and categorize them
        // This requires implementing type iteration and categorization

        // Merge implements relationships for object and interface types
        for object_type in &object_types {
            self.merge_implements(object_type);
        }

        for interface_type in &interface_types {
            self.merge_implements(interface_type);
        }

        // Merge union types
        for union_type in &union_types {
            self.merge_type_union(union_type);
        }

        // Merge schema definition (root types)
        self.merge_schema_definition();

        // Merge non-union and non-enum types
        for type_def in &non_union_enum_types {
            self.merge_type_general(type_def);
        }

        // Merge directive definitions
        self.merge_directive_definitions();

        // Merge enum types last
        for enum_type in &enum_types {
            self.merge_type_enum(enum_type);
        }

        // Validate that we have a query root type
        self.validate_query_root();

        // Merge all applied directives
        self.merge_all_applied_directives();

        // Add missing interface object fields to implementations
        self.add_missing_interface_object_fields_to_implementations();

        // Post-merge validations if no errors so far
        if !self.error_reporter.has_errors() {
            self.post_merge_validations();
        }

        // Return result
        let (errors, hints) = self.error_reporter.into_errors_and_hints();
        if !errors.is_empty() {
            MergeResult {
                supergraph: None,
                errors,
                hints,
            }
        } else {
            let valid_schema = Valid::assume_valid(self.merged);
            MergeResult {
                supergraph: Some(valid_schema),
                errors,
                hints,
            }
        }
    }

    // Methods called directly by merge() - implemented with todo!() for now

    fn validate_compose_directive_manager(&mut self) {
        todo!("Implement compose directive manager validation")
    }

    fn add_core_features(&mut self) {
        todo!("Implement adding core features to merged schema")
    }

    fn add_types_shallow(&mut self) {
        todo!("Implement shallow type addition - create empty type definitions")
    }

    fn add_directives_shallow(&mut self) {
        todo!("Implement shallow directive addition - create empty directive definitions")
    }

    fn merge_implements(&mut self, _type_def: &Name) {
        todo!("Implement merging of 'implements' relationships")
    }

    fn merge_type_union(&mut self, _union_type: &Name) {
        todo!("Implement union type merging")
    }

    fn merge_schema_definition(&mut self) {
        todo!("Implement schema definition merging (root types)")
    }

    fn merge_type_general(&mut self, _type_def: &Name) {
        todo!("Implement general type merging")
    }

    fn merge_directive_definitions(&mut self) {
        todo!("Implement directive definition merging")
    }

    fn merge_type_enum(&mut self, _enum_type: &Name) {
        todo!("Implement enum type merging - collect sources and call merge_enum")
    }

    fn validate_query_root(&mut self) {
        todo!("Implement query root validation")
    }

    fn merge_all_applied_directives(&mut self) {
        todo!("Implement merging of all applied directives")
    }

    fn add_missing_interface_object_fields_to_implementations(&mut self) {
        todo!("Implement adding missing interface object fields to implementations")
    }

    fn post_merge_validations(&mut self) {
        todo!("Implement post-merge validations")
    }

    /// Merge enum type from multiple subgraphs
    pub(crate) fn merge_enum(
        &mut self,
        sources: Sources<&EnumType>,
        dest: &EnumType,
    ) -> Result<(), FederationError> {
        let usage = self.enum_usages.get(dest.name.as_str()).cloned().unwrap_or_else(|| {
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
                    dest.name
                ),
            });
            usage
        });

        let mut enum_values: IndexSet<Name> = Default::default();

        enum_values.extend(
        sources
            .iter()
            .filter_map(|(_, source)| source.as_ref())
            .flat_map(|source| source.values.values())
            .map(|value| value.node.value.clone())
        );

        // Merge each enum value
        for value_name in enum_values {
            let value_pos = EnumValueDefinitionPosition {
                type_name: dest.name.clone(),
                value_name,
            };
            self.merge_enum_value(&sources, &value_pos, &usage)?;
        }

        let pos = EnumTypeDefinitionPosition {
            type_name: dest.name.clone(),
        };
        // We could be left with an enum type with no values, and that's invalid in graphQL
        if pos.get(&self.merged.schema())?.values.is_empty() {
            self.error_reporter.add_error(SingleFederationError::EmptyMergedEnumType {
                message: format!(
                    "None of the values of enum type \"{}\" are defined consistently in all the subgraphs defining that type. As only values common to all subgraphs are merged, this would result in an empty type.",
                    dest.name
                ),
            });
        }

        Ok(())
    }

    /// Merge a specific enum value across subgraphs
    /// Returns true if the value should be removed from the enum
    fn merge_enum_value(
        &mut self,
        sources: &Sources<&EnumType>,
        value_pos: &EnumValueDefinitionPosition,
        usage: &EnumTypeUsage,
    ) -> Result<(), FederationError> {
        // We merge directives (and description while at it) on the value even though we might remove it later in that function,
        // but we do so because:
        // 1. this will catch any problems merging the description/directives (which feels like a good thing).
        // 2. it easier to see if the value is marked @inaccessible.

        let value_sources: Sources<&Component<EnumValueDefinition>> = sources
            .iter()
            .map(|(&idx, s)| {
                let source_value =
                    s.and_then(|enum_type| enum_type.values.get(&value_pos.value_name));
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
        self.add_join_enum_value(&value_sources, &value_pos)?;

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
        if !is_inaccessible
            && !matches!(usage, EnumTypeUsage::Output { .. })
            && !matches!(usage, EnumTypeUsage::Unused)
            && sources.values().any(|source| {
                source.map_or(false, |enum_type| {
                    !enum_type.values.contains_key(&value_pos.value_name)
                })
            })
        {
            // We have a source (subgraph) that _has_ the enum type but not that particular enum value. If we're in the "both input and output usages",
            // that's where we have to fail. But if we're in the "only input" case, we simply don't merge that particular value and hint about it.
            match usage {
                EnumTypeUsage::Both {
                    input_example,
                    output_example,
                } => {
                    self.report_mismatch_error_with_specifics(
                        SingleFederationError::EnumValueMismatch {
                            message: format!(
                                "Enum type \"{}\" is used as both input type (for example, as type of \"{}\") and output type (for example, as type of \"{}\"), but value \"{}\" is not defined in all the subgraphs defining \"{}\": ",
                                &value_pos.type_name, input_example, output_example, &value_pos.value_name, &value_pos.type_name
                            ),
                        },
                        sources,
                        |source| {
                            source.map_or("no", |enum_type| {
                                if enum_type.values.contains_key(&value_pos.value_name) { "yes" } else { "no" }
                            })
                        },
                    );
                }
                EnumTypeUsage::Input { input_example } => {
                    self.report_mismatch_hint(
                        HintCode::InconsistentEnumValueForInputEnum,
                        format!(
                            "Value \"{}\" of enum type \"{}\" will not be part of the supergraph as it is not defined in all the subgraphs defining \"{}\": ",
                            &value_pos.value_name, &value_pos.type_name, &value_pos.type_name
                        ),
                        sources,
                        |source| {
                            source.map_or("no", |enum_type| {
                                if enum_type.values.contains_key(&value_pos.value_name) { "yes" } else { "no" }
                            })
                        },
                    );
                    value_pos.remove(&mut self.merged)?;
                }
                _ => todo!(),
            }
        } else if matches!(usage, EnumTypeUsage::Output { .. })
            || matches!(usage, EnumTypeUsage::Unused)
        {
            self.hint_on_inconsistent_output_enum_value(
                sources,
                &value_pos.type_name,
                &value_pos.value_name,
            );
        }
        Ok(())
    }

    // Helper functions that need to be implemented as stubs

    fn merge_description<T>(&mut self, _sources: &Sources<Option<T>>, _dest: &mut T) {
        todo!("Implement merge_description")
    }

    fn record_applied_directives_to_merge<T>(
        &mut self,
        _sources: &Sources<Option<T>>,
        _dest: &mut T,
    ) {
        todo!("Implement record_applied_directives_to_merge")
    }

    fn add_join_enum_value(
        &mut self,
        sources: &Sources<&Component<EnumValueDefinition>>,
        value_pos: &EnumValueDefinitionPosition,
    ) -> Result<(), FederationError> {
        if let Some(spec) = self.join_spec_definition.enum_value_directive_spec() {
            let dest = value_pos.get(&self.merged.schema())?;

            for (&idx, source) in sources.iter() {
                if source.is_some() {
                    // Get the join spec name for this subgraph
                    let subgraph_name = &self.names[idx];
                    let join_spec_name = self
                        .subgraph_names_to_join_spec_name
                        .get(subgraph_name)
                        .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Could not find join spec name for subgraph '{}'",
                            subgraph_name
                        ),
                    })?;

                    let directive = Node::new(Directive {
                        name: spec.name.clone(),
                        arguments: vec![Node::new(Argument {
                            name: name!(JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC),
                            value: Node::new(Value::Enum(name!(join_spec_name))),
                        })],
                    });
                    let value_pos = EnumValueDefinitionPosition {
                        type_name: value_pos.type_name.clone(),
                        value_name: value_pos.value_name.clone(),
                    };
                    value_pos.insert_directive(&mut self.merged, directive);
                }
            }
        }
        Ok(())
    }

    fn is_inaccessible_directive_in_supergraph(&self, _value: &EnumValueDefinition) -> bool {
        todo!("Implement is_inaccessible_directive_in_supergraph")
    }

    fn report_mismatch_error_with_specifics<T>(
        &mut self,
        error: SingleFederationError,
        sources: &Sources<T>,
        accessor: impl Fn(&Option<T>) -> &str,
    ) {
        // Build a detailed error message by showing which subgraphs have/don't have the element
        let mut details = Vec::new();
        let mut has_subgraphs = Vec::new();
        let mut missing_subgraphs = Vec::new();

        for (&idx, source) in sources.iter() {
            let subgraph_name = if idx < self.names.len() {
                &self.names[idx]
            } else {
                "unknown"
            };

            let result = accessor(source);
            if result == "yes" {
                has_subgraphs.push(subgraph_name);
            } else {
                missing_subgraphs.push(subgraph_name);
            }
        }

        // Format the subgraph lists
        if !has_subgraphs.is_empty() {
            details.push(format!("defined in {}", has_subgraphs.join(", ")));
        }
        if !missing_subgraphs.is_empty() {
            details.push(format!("but not in {}", missing_subgraphs.join(", ")));
        }

        // Create the enhanced error with details
        let enhanced_error = match error {
            SingleFederationError::EnumValueMismatch { message } => {
                SingleFederationError::EnumValueMismatch {
                    message: format!("{}{}", message, details.join(" ")),
                }
            }
            // Add other error types as needed
            other => other,
        };

        self.error_reporter.add_error(enhanced_error);
    }

    fn report_mismatch_hint<T>(
        &mut self,
        code: HintCode,
        message: String,
        _sources: &Sources<T>,
        _accessor: impl Fn(&Option<T>) -> &str,
    ) {
        // Stub implementation - just print the hint for now
        println!("HINT [{}]: {}", code.definition().code(), message);
    }

    fn hint_on_inconsistent_output_enum_value(
        &mut self,
        sources: &Sources<&EnumType>,
        dest_name: &Name,
        value_name: &Name,
    ) {
        // As soon as we find a subgraph that has the type but not the member, we hint.
        for enum_type in sources.values().flatten() {
            if !enum_type.values.contains_key(value_name) {
                self.report_mismatch_hint(
                    HintCode::InconsistentEnumValueForOutputEnum,
                    format!(
                        "Value \"{}\" of enum type \"{}\" has been added to the supergraph but is only defined in a subset of the subgraphs defining \"{}\": ",
                        value_name, dest_name, dest_name
                    ),
                    sources,
                    |source| {
                        source.map_or("no", |enum_type| {
                            if enum_type.values.contains_key(value_name) { "yes" } else { "no" }
                        })
                    },
                );
                return;
            }
        }
    }
}

// Public function to start the merging process
#[allow(dead_code)]
pub(crate) fn merge_subgraphs(
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
) -> Result<MergeResult, FederationError> {
    Ok(Merger::new(subgraphs, options)?.merge())
}

#[cfg(test)]
mod tests {
    use apollo_compiler::schema::ComponentOrigin;

    use super::*;
    use crate::error::ErrorCode;
    use crate::schema::position::EnumTypeDefinitionPosition;
    use crate::schema::position::PositionLookupError;
    use apollo_compiler::Node;
    use apollo_compiler::name;

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
    fn create_test_merger() -> Result<Merger, FederationError> {
        let join_spec_definition = JOIN_VERSIONS
            .find(&crate::link::spec::Version { major: 0, minor: 5 })
            .expect("JOIN_VERSIONS should have version 0.5");

        let mut schema = FederationSchema::new(Schema::new())?;
        insert_enum_type(&mut schema, name!("Status"))?;
        insert_enum_type(&mut schema, name!("UnusedStatus"))?;

        Ok(Merger {
            subgraphs: vec![],
            options: CompositionOptions::default(),
            names: vec!["subgraph1".to_string(), "subgraph2".to_string()],
            error_reporter: ErrorReporter::new(),
            merged: schema,
            subgraph_names_to_join_spec_name: [
                ("subgraph1".to_string(), "SUBGRAPH1".to_string()),
                ("subgraph2".to_string(), "SUBGRAPH2".to_string()),
            ]
            .into_iter()
            .collect(),
            merged_federation_directive_names: HashSet::new(),
            enum_usages: HashMap::new(),
            fields_with_from_context: HashSet::new(),
            fields_with_override: HashSet::new(),
            inaccessible_directive_name_in_supergraph: None,
            join_spec_definition,
        })
    }

    // Helper function to create enum type with values
    fn create_enum_type(name: &str, values: &[&str]) -> EnumType {
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

        enum_type
    }

    fn get_enum_values(
        merger: &Merger,
        enum_name: &str,
    ) -> Result<Vec<String>, PositionLookupError> {
        let enum_pos = EnumTypeDefinitionPosition {
            type_name: Name::new_unchecked(enum_name),
        };
        Ok(enum_pos
            .get(&merger.merged.schema())?
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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let dest = create_enum_type("Status", &[]);

        // Set up usage as output-only (union strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Output {
                output_example: "field1".to_string(),
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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("Status", &[]);

        // Set up usage as input-only (intersection strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Input {
                input_example: "field1".to_string(),
            },
        );

        // Merge should only include common values for input-only enum
        let result = merger.merge_enum(sources, &mut dest);

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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("Status", &[]);

        // Set up usage as both input and output (requires consistency)
        let usage = EnumTypeUsage::Both {
            input_example: "field1".to_string(),
            output_example: "field2".to_string(),
        };

        merger.enum_usages.insert("Status".to_string(), usage);

        // This should generate an error for inconsistent values
        let result = merger.merge_enum(sources, &mut dest);

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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("Status", &[]);

        // Set up usage as input-only (intersection strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Input {
                input_example: "field1".to_string(),
            },
        );

        let result = merger.merge_enum(sources, &mut dest);

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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("UnusedStatus", &[]);

        // Don't set usage - this should trigger the unused enum path
        // which treats it as output-only

        let result = merger.merge_enum(sources, &mut dest);

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

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("Status", &[]);

        // Set up usage as both input and output
        merger.enum_usages.insert(
            "Status".to_string(),
            EnumTypeUsage::Both {
                input_example: "field1".to_string(),
                output_example: "field2".to_string(),
            },
        );

        let result = merger.merge_enum(sources, &mut dest);

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
