use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::validation::Valid;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::join_spec_definition::JOIN_VERSIONS;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::compose_directive_manager::ComposeDirectiveManager;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::merger::merge_enum::EnumTypeUsage;
use crate::schema::FederationSchema;
use crate::schema::referencer::DirectiveReferencers;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

/// Type alias for Sources mapping - maps subgraph indices to optional values
pub(crate) type Sources<T> = IndexMap<usize, Option<T>>;

#[derive(Debug)]
pub(crate) struct MergeResult {
    #[allow(dead_code)]
    pub(crate) supergraph: Option<Valid<FederationSchema>>,
    #[allow(dead_code)]
    pub(crate) errors: Vec<CompositionError>,
    #[allow(dead_code)]
    pub(crate) hints: Vec<CompositionHint>,
}

#[derive(Debug, Default)]
pub(crate) struct CompositionOptions {
    // Add options as needed - for now keeping it minimal
}

#[allow(unused)]
pub(crate) struct Merger {
    pub(crate) subgraphs: Vec<Subgraph<Validated>>,
    pub(crate) options: CompositionOptions,
    pub(crate) compose_directive_manager: ComposeDirectiveManager,
    pub(crate) names: Vec<String>,
    pub(crate) error_reporter: ErrorReporter,
    pub(crate) merged: FederationSchema,
    pub(crate) subgraph_names_to_join_spec_name: HashMap<String, Name>,
    pub(crate) merged_federation_directive_names: HashSet<String>,
    pub(crate) enum_usages: HashMap<String, EnumTypeUsage>,
    pub(crate) fields_with_from_context: DirectiveReferencers,
    pub(crate) fields_with_override: DirectiveReferencers,
    pub(crate) inaccessible_directive_name_in_supergraph: Option<Name>,
    pub(crate) schema_to_import_to_feature_url: HashMap<String, HashMap<String, Url>>,
    pub(crate) join_directive_identities: HashSet<Identity>,
    pub(crate) join_spec_definition: &'static JoinSpecDefinition,
}

#[allow(unused)]
impl Merger {
    pub(crate) fn new(
        subgraphs: Vec<Subgraph<Validated>>,
        options: CompositionOptions,
    ) -> Result<Self, FederationError> {
        let mut error_reporter = ErrorReporter::new();
        let latest_federation_version_used =
            Self::get_latest_federation_version_used(&subgraphs, &mut error_reporter);
        let join_spec = JOIN_VERSIONS.get_minimum_required_version(latest_federation_version_used);
        let link_spec = LINK_VERSIONS.get_minimum_required_version(latest_federation_version_used);
        let fields_with_from_context = Self::get_fields_with_from_context_directive(&subgraphs);
        let fields_with_override = Self::get_fields_with_override_directive(&subgraphs);

        let names: Vec<String> = subgraphs.iter().map(|s| s.name.clone()).collect();
        let schema_to_import_to_feature_url = subgraphs
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.schema()
                        .metadata()
                        .map(|l| l.import_to_feature_url_map())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let subgraph_names_to_join_spec_name = Self::prepare_supergraph()?;
        let join_directive_identities = HashSet::from([Identity::connect_identity()]);

        Ok(Self {
            subgraphs,
            options,
            names,
            compose_directive_manager: ComposeDirectiveManager::new(),
            error_reporter,
            merged: FederationSchema::new(Schema::new())?,
            subgraph_names_to_join_spec_name,
            merged_federation_directive_names: todo!(),
            enum_usages: HashMap::new(),
            fields_with_from_context,
            fields_with_override,
            schema_to_import_to_feature_url,
            join_directive_identities,
            inaccessible_directive_name_in_supergraph: todo!(),
            join_spec_definition: join_spec.expect("exists"), // TODO: handle this and bail up top
        })
    }

    /// Get the join spec name for a subgraph by index (ported from JavaScript joinSpecName())
    pub(crate) fn join_spec_name(&self, subgraph_index: usize) -> Result<&Name, FederationError> {
        let subgraph_name = &self.names[subgraph_index];
        self.subgraph_names_to_join_spec_name
            .get(subgraph_name)
            .ok_or_else(|| {
                internal_error!(
                    "Could not find join spec name for subgraph '{}'",
                    subgraph_name
                )
            })
    }

    fn get_latest_federation_version_used<'a>(
        subgraphs: &'a [Subgraph<Validated>],
        error_reporter: &mut ErrorReporter,
    ) -> &'a Version {
        subgraphs
            .iter()
            .map(|subgraph| {
                Self::get_latest_federation_version_used_in_subgraph(subgraph, error_reporter)
            })
            .max()
            .unwrap_or_else(|| FEDERATION_VERSIONS.latest().version())
    }

    fn get_latest_federation_version_used_in_subgraph<'a>(
        subgraph: &'a Subgraph<Validated>,
        error_reporter: &mut ErrorReporter,
    ) -> &'a Version {
        let linked_federation_version = subgraph.metadata().federation_spec_definition().version();

        let linked_features = subgraph.schema().all_features().unwrap_or_default();
        let spec_with_max_implied_version = linked_features
            .iter()
            .max_by_key(|spec| spec.minimum_federation_version());

        if let Some(spec) = spec_with_max_implied_version {
            if spec
                .minimum_federation_version()
                .satisfies(linked_federation_version)
                && spec
                    .minimum_federation_version()
                    .gt(linked_federation_version)
            {
                error_reporter.add_hint(CompositionHint {
                    code: HintCode::ImplicitlyUpgradedFederationVersion
                        .code()
                        .to_string(),
                    message: format!(
                        "Subgraph {} has been implicitly upgraded from federation {} to {}",
                        subgraph.name,
                        linked_federation_version,
                        spec.minimum_federation_version()
                    ),
                });
                return spec.minimum_federation_version();
            }
        }
        linked_federation_version
    }

    fn get_fields_with_from_context_directive(
        subgraphs: &[Subgraph<Validated>],
    ) -> DirectiveReferencers {
        subgraphs
            .iter()
            .fold(Default::default(), |mut acc, subgraph| {
                if let Ok(Some(directive_name)) = subgraph.from_context_directive_name() {
                    if let Ok(referencers) = subgraph
                        .schema()
                        .referencers()
                        .get_directive(&directive_name)
                    {
                        acc.extend(referencers);
                    }
                }
                acc
            })
    }

    fn get_fields_with_override_directive(
        subgraphs: &[Subgraph<Validated>],
    ) -> DirectiveReferencers {
        subgraphs
            .iter()
            .fold(Default::default(), |mut acc, subgraph| {
                if let Ok(Some(directive_name)) = subgraph.override_directive_name() {
                    if let Ok(referencers) = subgraph
                        .schema()
                        .referencers()
                        .get_directive(&directive_name)
                    {
                        acc.extend(referencers);
                    }
                }
                acc
            })
    }

    fn prepare_supergraph() -> Result<HashMap<String, Name>, FederationError> {
        todo!("Prepare supergraph")
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

    fn is_inaccessible_directive_in_supergraph(&self, _value: &EnumValueDefinition) -> bool {
        todo!("Implement is_inaccessible_directive_in_supergraph")
    }

    // TODO: These error reporting functions are not yet fully implemented
    pub(crate) fn report_mismatch_error_with_specifics<T>(
        &mut self,
        error: CompositionError,
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
            CompositionError::EnumValueMismatch { message } => {
                CompositionError::EnumValueMismatch {
                    message: format!("{}{}", message, details.join(" ")),
                }
            }
            // Add other error types as needed
            other => other,
        };

        self.error_reporter.add_error(enhanced_error);
    }

    pub(crate) fn report_mismatch_hint<T>(
        &mut self,
        code: HintCode,
        message: String,
        sources: &Sources<T>,
        accessor: impl Fn(&Option<T>) -> bool,
    ) {
        // Build detailed hint message showing which subgraphs have/don't have the element
        let mut has_subgraphs = Vec::new();
        let mut missing_subgraphs = Vec::new();

        for (&idx, source) in sources.iter() {
            let subgraph_name = if idx < self.names.len() {
                &self.names[idx]
            } else {
                "unknown"
            };
            let result = accessor(source);
            if result {
                has_subgraphs.push(subgraph_name);
            } else {
                missing_subgraphs.push(subgraph_name);
            }
        }

        let detailed_message = format!(
            "{}defined in {} but not in {}",
            message,
            has_subgraphs.join(", "),
            missing_subgraphs.join(", ")
        );

        // Add the hint to the error reporter
        let hint = CompositionHint {
            code: code.definition().code().to_string(),
            message: detailed_message,
        };
        self.error_reporter.add_hint(hint);
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
