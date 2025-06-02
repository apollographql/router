use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;

use crate::JOIN_VERSIONS;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::compose_directive_manager::ComposeDirectiveManager;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::schema::referencer::DirectiveReferencers;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

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

#[allow(unused)]
pub(crate) struct Merger {
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
    names: Vec<String>,
    compose_directive_manager: ComposeDirectiveManager,
    error_reporter: ErrorReporter,
    merged: Schema,
    subgraph_names_to_join_spec_name: HashMap<String, String>,
    merged_federation_directive_names: HashSet<String>,
    enum_usages: HashMap<String, String>, // Simplified for now
    fields_with_from_context: DirectiveReferencers,
    fields_with_override: DirectiveReferencers,
    schema_to_import_to_feature_url: HashMap<String, HashMap<String, Url>>,
    join_directive_identities: HashSet<Identity>,
}

#[allow(unused)]
impl Merger {
    pub(crate) fn new(subgraphs: Vec<Subgraph<Validated>>, options: CompositionOptions) -> Self {
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
        let subgraph_names_to_join_spec_name = Self::prepare_supergraph();
        let join_directive_identities = HashSet::from([Identity::connect_identity()]);

        Self {
            subgraphs,
            options,
            names,
            compose_directive_manager: ComposeDirectiveManager::new(),
            error_reporter,
            merged: Schema::new(),
            subgraph_names_to_join_spec_name,
            merged_federation_directive_names: todo!(),
            enum_usages: HashMap::new(),
            fields_with_from_context,
            fields_with_override,
            schema_to_import_to_feature_url,
            join_directive_identities,
        }
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
        let spec_with_max_implied_version = linked_features.iter().reduce(|a, b| {
            if a.minimum_federation_version()
                .gt(b.minimum_federation_version())
            {
                a
            } else {
                b
            }
        });

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

    fn prepare_supergraph() -> HashMap<String, String> {
        // Note: this likely has to return a Result, which will also change the signature of Merger::new
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
        todo!("Implement enum type merging")
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
}

// Public function to start the merging process
#[allow(dead_code)]
pub(crate) fn merge_subgraphs(
    subgraphs: Vec<Subgraph<Validated>>,
    options: CompositionOptions,
) -> MergeResult {
    Merger::new(subgraphs, options).merge()
}
