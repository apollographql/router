use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Schema;
use apollo_compiler::Name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::validation::Valid;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::inaccessible_spec_definition::IsInaccessibleExt;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
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
    pub(crate) supergraph: Option<Valid<Schema>>,
    #[allow(dead_code)]
    pub(crate) errors: Vec<SingleFederationError>,
    #[allow(dead_code)]
    pub(crate) hints: Vec<CompositionHint>,
}

#[derive(Debug)]
pub(crate) struct CompositionOptions {
    // Add options as needed - for now keeping it minimal
}

impl Default for CompositionOptions {
    fn default() -> Self {
        Self {}
    }
}

#[allow(unused)]
pub(crate) struct Merger {
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
    names: Vec<String>,
    error_reporter: ErrorReporter,
    merged: Schema,
    subgraph_names_to_join_spec_name: HashMap<String, String>,
    merged_federation_directive_names: HashSet<String>,
    enum_usages: HashMap<String, EnumTypeUsage>,
    fields_with_from_context: HashSet<String>,
    fields_with_override: HashSet<String>,
    inaccessible_directive_name_in_supergraph: Option<Name>,
}

#[allow(unused)]
impl Merger {
    pub(crate) fn new(subgraphs: Vec<Subgraph<Validated>>, options: CompositionOptions) -> Self {
        let names: Vec<String> = subgraphs.iter().map(|s| s.name.clone()).collect();

        Self {
            subgraphs,
            options,
            names,
            error_reporter: ErrorReporter::new(),
            merged: Schema::new(),
            subgraph_names_to_join_spec_name: todo!(),
            merged_federation_directive_names: todo!(),
            enum_usages: HashMap::new(),
            fields_with_from_context: todo!(),
            fields_with_override: todo!(),
            inaccessible_directive_name_in_supergraph: todo!(),
        }
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
    pub(crate) fn merge_enum(&mut self, sources: Sources<&EnumType>, dest: &mut EnumType) -> Result<(), FederationError> {
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
                let source_value = s.and_then(|enum_type| {
                    enum_type.values.get(&value.node.value).map(|v| v)
                });
                (idx, source_value)
            })
            .collect();
        
        
        // TODO: Implement these helper methods - for now skip the actual merging
        // self.merge_description(&value_sources, &mut value.node);
        // self.record_applied_directives_to_merge(&value_sources, &mut value.node);
        // self.add_join_enum_value(&value_sources, &mut value.node);

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
                let input_example = usage.examples.get(&EnumUsagePosition::Input)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown field");
                let output_example = usage.examples.get(&EnumUsagePosition::Output)
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
                        if let Some(enum_type) = source {
                            if enum_type.values.contains_key(&value.node.value) { "yes" } else { "no" }
                        } else {
                            "no"
                        }
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
                        if let Some(enum_type) = source {
                            if enum_type.values.contains_key(&value.node.value) { "yes" } else { "no" }
                        } else {
                            "no"
                        }
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

    fn record_applied_directives_to_merge<T>(&mut self, _sources: &Sources<Option<T>>, _dest: &mut T) {
        todo!("Implement record_applied_directives_to_merge")
    }

    fn add_join_enum_value(&mut self, _sources: &Sources<Option<&EnumValueDefinition>>, _dest: &mut EnumValueDefinition) {
        todo!("Implement add_join_enum_value")
    }

    fn is_inaccessible_directive_in_supergraph(&self, _value: &EnumValueDefinition) -> bool {
        todo!("Implement is_inaccessible_directive_in_supergraph")
    }

    fn some_sources<T>(&self, sources: &Sources<T>, predicate: impl Fn(&Option<T>) -> bool) -> bool {
        sources.values().any(predicate)
    }

    fn report_mismatch_error_with_specifics<T>(
        &mut self,
        _error: SingleFederationError,
        _sources: &Sources<T>,
        _accessor: impl Fn(&Option<T>) -> &str,
    ) {
        todo!("Implement report_mismatch_error_with_specifics")
    }

    fn report_mismatch_hint<T>(
        &mut self,
        _code: HintCode,
        _message: String,
        _sources: &Sources<T>,
        _accessor: impl Fn(&Option<T>) -> &str,
    ) {
        todo!("Implement report_mismatch_hint")
    }

    fn hint_on_inconsistent_output_enum_value(
        &mut self,
        _sources: &Sources<&EnumType>,
        _dest_name: &Name,
        _value_name: &Name,
    ) {
        todo!("Implement hint_on_inconsistent_output_enum_value")
    }
}

// Public function to start the merging process
#[allow(dead_code)]
pub(crate) fn merge_subgraphs(
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
) -> MergeResult {
    Merger::new(subgraphs, options).merge()
}