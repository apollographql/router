use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Schema;
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
use crate::schema::position::EnumValueDefinitionPosition;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

/// Type alias for Sources mapping - maps subgraph indices to optional values
type Sources<T> = HashMap<usize, Option<T>>;

/// Enum usage position tracking
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum EnumUsagePosition {
    Input,
    Output,
    Both,
}

/// Tracks how an enum type is used across the schema
#[derive(Debug, Clone)]
pub(crate) struct EnumTypeUsage {
    pub position: EnumUsagePosition,
    pub examples: HashMap<EnumUsagePosition, String>, // Example field coordinates
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
        dest: &mut EnumType,
    ) -> Result<(), FederationError> {
        let usage = self.enum_usages.get(&dest.name.to_string()).cloned().unwrap_or_else(|| {
            // If the enum is unused, we have a choice to make. We could skip the enum entirely (after all, exposing an unreferenced type mostly "pollutes" the supergraph API), but
            // some evidence shows that many a user have such unused enums in federation 1 and having those removed from their API might be surprising. We could merge it as
            // an "input-only" or as a "input/ouput" type, but the hints/errors generated in both those cases would be confusing in that case, and while we could amend them
            // for this case, it would complicate things and doesn't feel like it would feel very justified. So we merge it as an "output" type, which is the least contraining
            // option. We do raise an hint though so users can notice this.
            let usage = EnumTypeUsage {
                position: EnumUsagePosition::Output,
                examples: HashMap::new(),
            };
            self.error_reporter.add_hint(CompositionHint {
                code: HintCode::UnusedEnumType.code().to_string(),
                message: format!(
                    "Enum type \"{}\" is defined but unused. It will be included in the supergraph with all the values appearing in any subgraph (\"as if\" it was only used as an output type).",
                    dest.name
                ),
            });
            usage
        });

        // Add all values from all sources
        for (_, source) in sources.iter() {
            if let Some(source) = source {
                for value in source.values.values() {
                    // Note that we add all the values we see as a simple way to know which values there is to consider. But some of those value may
                    // be removed later in `merge_enum_value`
                    if !dest.values.contains_key(&value.node.value) {
                        dest.values.insert(value.node.value.clone(), value.clone());
                    }
                }
            }
        }

        // Merge each enum value
        let value_names: Vec<Name> = dest.values.keys().cloned().collect();
        let mut values_to_remove = Vec::new();
        for value_name in value_names {
            if let Some(value) = dest.values.get_mut(&value_name) {
                let should_remove = self.merge_enum_value(&sources, &dest.name, value, &usage)?;
                if should_remove {
                    values_to_remove.push(value_name);
                }
            }
        }

        // Remove values that were marked for removal
        for value_name in values_to_remove {
            dest.values.shift_remove(&value_name);
        }

        // We could be left with an enum type with no values, and that's invalid in graphQL
        if dest.values.is_empty() {
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
        dest_name: &Name,
        value: &mut Component<EnumValueDefinition>,
        usage: &EnumTypeUsage,
    ) -> Result<bool, FederationError> {
        // We merge directives (and description while at it) on the value even though we might remove it later in that function,
        // but we do so because:
        // 1. this will catch any problems merging the description/directives (which feels like a good thing).
        // 2. it easier to see if the value is marked @inaccessible.

        let value_sources: Sources<&Component<EnumValueDefinition>> = sources
            .iter()
            .map(|(&idx, s)| {
                let source_value = s.and_then(|enum_type| enum_type.values.get(&value.node.value));
                (idx, source_value)
            })
            .collect();

        // TODO: Implement these helper methods - for now skip the actual merging
        // self.merge_description(&value_sources, &mut value.node);
        // self.record_applied_directives_to_merge(&value_sources, &mut value.node);
        self.add_join_enum_value(&value_sources, value)?;

        let value_pos = EnumValueDefinitionPosition {
            type_name: dest_name.clone(),
            value_name: value.value.clone(),
        };
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
            && usage.position != EnumUsagePosition::Output
            && sources.values().any(|source| {
                if let Some(enum_type) = source {
                    !enum_type.values.contains_key(&value.node.value)
                } else {
                    false
                }
            })
        {
            // We have a source (subgraph) that _has_ the enum type but not that particular enum value. If we're in the "both input and output usages",
            // that's where we have to fail. But if we're in the "only input" case, we simply don't merge that particular value and hint about it.
            if usage.position == EnumUsagePosition::Both {
                let input_example = usage
                    .examples
                    .get(&EnumUsagePosition::Input)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown field");
                let output_example = usage
                    .examples
                    .get(&EnumUsagePosition::Output)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown field");
                self.report_mismatch_error_with_specifics(
                    SingleFederationError::EnumValueMismatch {
                        message: format!(
                            "Enum type \"{}\" is used as both input type (for example, as type of \"{}\") and output type (for example, as type of \"{}\"), but value \"{}\" is not defined in all the subgraphs defining \"{}\": ",
                            dest_name, input_example, output_example, value.node.value, dest_name
                        ),
                    },
                    sources,
                    |source| {
                        source.map_or("no", |enum_type| {
                            if enum_type.values.contains_key(&value.node.value) { "yes" } else { "no" }
                        })
                    },
                );
                // We leave the value in the merged output in that case because:
                // 1. it's harmless to do so; we have an error so we won't return a supergraph.
                // 2. it avoids generating an additional "enum type is empty" error in `merge_enum` if all the values are inconsistent.
            } else {
                self.report_mismatch_hint(
                    HintCode::InconsistentEnumValueForInputEnum,
                    format!(
                        "Value \"{}\" of enum type \"{}\" will not be part of the supergraph as it is not defined in all the subgraphs defining \"{}\": ",
                        value.node.value, dest_name, dest_name
                    ),
                    sources,
                    |source| {
                        source.map_or("no", |enum_type| {
                            if enum_type.values.contains_key(&value.node.value) { "yes" } else { "no" }
                        })
                    },
                );
                // We remove the value after the generation of the hint/errors because `report_mismatch_hint` will show the message for the subgraphs that are "like" the supergraph
                // first, and the message flows better if we say which subgraph defines the value first, so we want the value to still be present for the generation of the
                // message.
                return Ok(true); // Indicate that this value should be removed
            }
        } else if usage.position == EnumUsagePosition::Output {
            self.hint_on_inconsistent_output_enum_value(sources, dest_name, &value.node.value);
        }
        Ok(false) // Don't remove the value
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
        dest: &mut Component<EnumValueDefinition>,
    ) -> Result<(), FederationError> {
        for (&idx, source) in sources.iter() {
            if source.is_none() {
                continue;
            }

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

            self.join_spec_definition
                .add_join_enum_value(dest, join_spec_name)?;
        }
        Ok(())
    }

    fn is_inaccessible_directive_in_supergraph(&self, _value: &EnumValueDefinition) -> bool {
        todo!("Implement is_inaccessible_directive_in_supergraph")
    }

    fn some_sources<T>(
        &self,
        sources: &Sources<T>,
        predicate: impl Fn(&Option<T>) -> bool,
    ) -> bool {
        sources.values().any(predicate)
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
    use apollo_compiler::Node;
    use apollo_compiler::name;

    // Helper function to create a minimal merger instance for testing
    // This only initializes what's needed for merge_enum() testing
    fn create_test_merger() -> Result<Merger, FederationError> {
        let join_spec_definition = JOIN_VERSIONS
            .find(&crate::link::spec::Version { major: 0, minor: 5 })
            .expect("JOIN_VERSIONS should have version 0.5");

        Ok(Merger {
            subgraphs: vec![],
            options: CompositionOptions::default(),
            names: vec!["subgraph1".to_string(), "subgraph2".to_string()],
            error_reporter: ErrorReporter::new(),
            merged: FederationSchema::new(Schema::new())?,
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

    // Helper function to create usage position
    fn create_usage(position: EnumUsagePosition) -> EnumTypeUsage {
        EnumTypeUsage {
            position,
            examples: HashMap::new(),
        }
    }

    #[test]
    fn test_merge_enum_output_only_enum_includes_all_values() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE", "PENDING"]);

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let mut dest = create_enum_type("Status", &[]);

        // Set up usage as output-only (union strategy)
        merger.enum_usages.insert(
            "Status".to_string(),
            create_usage(EnumUsagePosition::Output),
        );

        // Merge should include all values from all subgraphs for output-only enum
        let result = merger.merge_enum(sources, &mut dest);

        assert!(result.is_ok());
        assert_eq!(dest.values.len(), 3); // ACTIVE, INACTIVE, PENDING
        assert!(dest.values.contains_key(&name!("ACTIVE")));
        assert!(dest.values.contains_key(&name!("INACTIVE")));
        assert!(dest.values.contains_key(&name!("PENDING")));
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
        merger
            .enum_usages
            .insert("Status".to_string(), create_usage(EnumUsagePosition::Input));

        // Merge should only include common values for input-only enum
        let result = merger.merge_enum(sources, &mut dest);

        assert!(result.is_ok());
        // Only ACTIVE should remain (intersection)
        // INACTIVE and PENDING should be removed with hints
        assert_eq!(dest.values.len(), 1);
        assert!(dest.values.contains_key(&name!("ACTIVE")));
        assert!(!dest.values.contains_key(&name!("INACTIVE")));
        assert!(!dest.values.contains_key(&name!("PENDING")));
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
        let mut usage = create_usage(EnumUsagePosition::Both);
        usage
            .examples
            .insert(EnumUsagePosition::Input, "field1".to_string());
        usage
            .examples
            .insert(EnumUsagePosition::Output, "field2".to_string());

        merger.enum_usages.insert("Status".to_string(), usage);

        // This should generate an error for inconsistent values
        let result = merger.merge_enum(sources, &mut dest);

        // The function should complete but the error reporter should have errors
        assert!(result.is_ok());
        // Both inconsistent values should still be present to avoid additional empty enum error
        assert!(dest.values.contains_key(&name!("ACTIVE")));
        // The error reporter should capture the inconsistency error
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
        merger
            .enum_usages
            .insert("Status".to_string(), create_usage(EnumUsagePosition::Input));

        let result = merger.merge_enum(sources, &mut dest);

        assert!(result.is_ok());
        // Should be empty after merging
        assert_eq!(dest.values.len(), 0);
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
        assert_eq!(dest.values.len(), 3); // ACTIVE, INACTIVE, PENDING
        assert!(dest.values.contains_key(&name!("ACTIVE")));
        assert!(dest.values.contains_key(&name!("INACTIVE")));
        assert!(dest.values.contains_key(&name!("PENDING")));
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
        merger
            .enum_usages
            .insert("Status".to_string(), create_usage(EnumUsagePosition::Both));

        let result = merger.merge_enum(sources, &mut dest);

        assert!(result.is_ok());
        // Should include all values since they're consistent
        assert_eq!(dest.values.len(), 3);
        assert!(dest.values.contains_key(&name!("ACTIVE")));
        assert!(dest.values.contains_key(&name!("INACTIVE")));
        assert!(dest.values.contains_key(&name!("PENDING")));
        // Should not generate any errors or hints
    }

    #[test]
    fn test_merge_enum_value_adds_join_directive() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types from different subgraphs
        let enum1 = create_enum_type("Status", &["ACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE"]);

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let dest_name = name!("Status");
        let mut value = enum1.values.get(&name!("ACTIVE")).unwrap().clone();
        let usage = create_usage(EnumUsagePosition::Output);

        let result = merger.merge_enum_value(&sources, &dest_name, &mut value, &usage);

        assert!(result.is_ok());
        let should_remove = result.unwrap();
        assert!(!should_remove); // Value should not be removed

        // Check that @join__enumValue directives were added
        // This would require the merger to have proper subgraph name mappings
        // which are currently todo!(), so this test will panic before reaching here
    }

    #[test]
    fn test_merge_enum_value_input_enum_removes_inconsistent_values() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types where one subgraph is missing the value
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE"]); // Missing INACTIVE

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let dest_name = name!("Status");
        let mut value = enum1.values.get(&name!("INACTIVE")).unwrap().clone();
        let usage = create_usage(EnumUsagePosition::Input);

        let result = merger.merge_enum_value(&sources, &dest_name, &mut value, &usage);

        assert!(result.is_ok());
        let should_remove = result.unwrap();
        assert!(should_remove); // INACTIVE should be removed for input enum
    }

    #[test]
    fn test_merge_enum_value_output_enum_keeps_all_values() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create enum types where one subgraph is missing the value
        let enum1 = create_enum_type("Status", &["ACTIVE", "INACTIVE"]);
        let enum2 = create_enum_type("Status", &["ACTIVE"]); // Missing INACTIVE

        let sources: Sources<&EnumType> =
            [(0, Some(&enum1)), (1, Some(&enum2))].into_iter().collect();

        let dest_name = name!("Status");
        let mut value = enum1.values.get(&name!("INACTIVE")).unwrap().clone();
        let usage = create_usage(EnumUsagePosition::Output);

        let result = merger.merge_enum_value(&sources, &dest_name, &mut value, &usage);

        assert!(result.is_ok());
        let should_remove = result.unwrap();
        assert!(!should_remove); // INACTIVE should be kept for output enum
    }

    #[test]
    fn test_add_join_enum_value_with_supported_version() {
        let mut merger = create_test_merger().expect("valid Merger object");

        // Create test enum values - separate instances to avoid borrowing conflicts
        let enum_type1 = create_enum_type("Status", &["ACTIVE"]);
        let enum_type2 = create_enum_type("Status", &["ACTIVE"]);
        let value1 = enum_type1.values.get(&name!("ACTIVE")).unwrap();
        let value2 = enum_type2.values.get(&name!("ACTIVE")).unwrap();

        // Create a separate mutable value for the function call
        let mut dest_value = value1.clone();

        // Create sources mapping - this would normally be populated by the merger
        let sources: Sources<&Component<EnumValueDefinition>> =
            [(0, Some(value1)), (1, Some(value2))].into_iter().collect();

        let result = merger.add_join_enum_value(&sources, &mut dest_value);

        // This should work if the join spec version is >= 0.3
        // But will panic due to todo!() in subgraph_names_to_join_spec_name access
        assert!(result.is_ok());
    }
}
