use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use countmap::CountMap;
use indexmap::IndexSet;
use itertools::Itertools;

use crate::LinkSpecDefinition;
use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SubgraphLocation;
use crate::internal_error;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Link;
use crate::link::federation_spec_definition::FEDERATION_OPERATION_TYPES;
use crate::link::federation_spec_definition::FEDERATION_VERSIONS;
use crate::link::join_spec_definition::EnumValue;
use crate::link::join_spec_definition::JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::join_spec_definition::JOIN_VERSIONS;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::link_spec_definition::LINK_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SPEC_REGISTRY;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::compose_directive_manager::ComposeDirectiveManager;
use crate::merger::error_reporter::ErrorReporter;
use crate::merger::hints::HintCode;
use crate::merger::merge_directive::AppliedDirectivesToMerge;
use crate::merger::merge_enum::EnumExample;
use crate::merger::merge_enum::EnumExampleAst;
use crate::merger::merge_enum::EnumTypeUsage;
use crate::merger::merge_field::FieldMergeContext;
use crate::schema::FederationSchema;
use crate::schema::directive_location::DirectiveLocationExt;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::HasDescription;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::SchemaDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::referencer::DirectiveReferencers;
use crate::schema::type_and_directive_specification::ArgumentMerger;
use crate::schema::type_and_directive_specification::StaticArgumentsTransform;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;
use crate::utils::human_readable::human_readable_subgraph_names;
use crate::utils::iter_into_single_item;

static NON_MERGED_CORE_FEATURES: LazyLock<[Identity; 4]> = LazyLock::new(|| {
    [
        Identity::federation_identity(),
        Identity::link_identity(),
        Identity::core_identity(),
        Identity::connect_identity(),
    ]
});

/// In JS, this is encoded indirectly in `isGraphQLBuiltInDirective`. Regardless of whether
/// the end user redefined these directives, we consider them built-in for merging.
static BUILT_IN_DIRECTIVES: [&str; 6] = [
    "skip",
    "include",
    "deprecated",
    "specifiedBy",
    "defer",
    "stream",
];

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

pub(in crate::merger) struct MergedDirectiveInfo {
    pub(in crate::merger) definition: DirectiveDefinition,
    pub(in crate::merger) arguments_merger: Option<ArgumentMerger>,
    pub(in crate::merger) static_argument_transform: Option<Rc<StaticArgumentsTransform>>,
}

#[derive(Debug, Default)]
pub(crate) struct CompositionOptions {
    // Add options as needed - for now keeping it minimal
    /// Maximum allowable number of outstanding subgraph paths to validate during satisfiability.
    pub(crate) max_validation_subgraph_paths: Option<usize>,
}

#[allow(unused)]
pub(crate) struct Merger {
    pub(in crate::merger) subgraphs: Vec<Subgraph<Validated>>,
    pub(in crate::merger) options: CompositionOptions,
    pub(in crate::merger) compose_directive_manager: ComposeDirectiveManager,
    pub(in crate::merger) names: Vec<String>,
    pub(in crate::merger) error_reporter: ErrorReporter,
    pub(in crate::merger) merged: FederationSchema,
    pub(in crate::merger) subgraph_names_to_join_spec_name: HashMap<String, Name>,
    pub(in crate::merger) merged_federation_directive_names: HashSet<String>,
    pub(in crate::merger) merged_federation_directive_in_supergraph_by_directive_name:
        HashMap<Name, MergedDirectiveInfo>,
    pub(in crate::merger) enum_usages: HashMap<String, EnumTypeUsage>,
    pub(in crate::merger) fields_with_from_context: DirectiveReferencers,
    pub(in crate::merger) fields_with_override: DirectiveReferencers,
    pub(in crate::merger) subgraph_enum_values: Vec<EnumValue>,
    pub(in crate::merger) inaccessible_directive_name_in_supergraph: Option<Name>,
    pub(in crate::merger) schema_to_import_to_feature_url: HashMap<String, HashMap<String, Url>>,
    pub(in crate::merger) link_spec_definition: &'static LinkSpecDefinition,
    pub(in crate::merger) join_directive_identities: HashSet<Identity>,
    pub(in crate::merger) join_spec_definition: &'static JoinSpecDefinition,
    pub(in crate::merger) latest_federation_version_used: Version,
    pub(in crate::merger) applied_directives_to_merge: AppliedDirectivesToMerge,
}

/// Abstraction for schema elements that have types that can be merged.
///
/// This replaces the TypeScript `NamedSchemaElementWithType` interface,
/// providing a unified way to handle type merging for both field definitions
/// and input value definitions (arguments).
pub(crate) trait SchemaElementWithType {
    //
    fn coordinate(&self, parent_name: &str) -> String;
    fn set_type(&mut self, typ: Type);
    fn enum_example_ast(&self) -> Option<EnumExampleAst>;
}

impl SchemaElementWithType for FieldDefinition {
    fn coordinate(&self, parent_name: &str) -> String {
        format!("{}.{}", parent_name, self.name)
    }
    fn set_type(&mut self, typ: Type) {
        self.ty = typ;
    }
    fn enum_example_ast(&self) -> Option<EnumExampleAst> {
        Some(EnumExampleAst::Field(Node::new(self.clone())))
    }
}

impl SchemaElementWithType for InputValueDefinition {
    fn coordinate(&self, parent_name: &str) -> String {
        format!("{}.{}", parent_name, self.name)
    }
    fn set_type(&mut self, typ: Type) {
        self.ty = typ.into();
    }
    fn enum_example_ast(&self) -> Option<EnumExampleAst> {
        Some(EnumExampleAst::Input(Node::new(self.clone())))
    }
}

#[allow(unused)]
impl Merger {
    pub(crate) fn new(
        subgraphs: Vec<Subgraph<Validated>>,
        options: CompositionOptions,
    ) -> Result<Self, FederationError> {
        let names: Vec<String> = subgraphs.iter().map(|s| s.name.clone()).collect();
        let mut error_reporter = ErrorReporter::new(names.clone());
        let latest_federation_version_used =
            Self::get_latest_federation_version_used(&subgraphs, &mut error_reporter).clone();
        let Some(join_spec) =
            JOIN_VERSIONS.get_minimum_required_version(&latest_federation_version_used)
        else {
            bail!(
                "No join spec version found for federation version {}",
                latest_federation_version_used
            )
        };
        let Some(link_spec_definition) =
            LINK_VERSIONS.get_minimum_required_version(&latest_federation_version_used)
        else {
            bail!(
                "No link spec version found for federation version {}",
                latest_federation_version_used
            )
        };
        let fields_with_from_context = Self::get_fields_with_from_context_directive(&subgraphs);
        let fields_with_override = Self::get_fields_with_override_directive(&subgraphs);

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
        let merged = FederationSchema::new(Schema::new())?;
        let join_directive_identities = HashSet::from([Identity::connect_identity()]);

        let mut merger = Self {
            subgraphs,
            options,
            names,
            compose_directive_manager: ComposeDirectiveManager::new(),
            error_reporter,
            merged,
            subgraph_names_to_join_spec_name: HashMap::new(),
            merged_federation_directive_names: HashSet::new(),
            merged_federation_directive_in_supergraph_by_directive_name: HashMap::new(),
            enum_usages: HashMap::new(),
            fields_with_from_context,
            fields_with_override,
            schema_to_import_to_feature_url,
            link_spec_definition,
            join_directive_identities,
            inaccessible_directive_name_in_supergraph: None,
            join_spec_definition: join_spec,
            subgraph_enum_values: Vec::new(),
            latest_federation_version_used,
            applied_directives_to_merge: Vec::new(),
        };

        // Now call prepare_supergraph as a member function
        merger.prepare_supergraph()?;

        Ok(merger)
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

        if let Some(spec) = spec_with_max_implied_version
            && spec
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
                locations: Default::default(), // TODO: need @link directive application AST node
            });
            return spec.minimum_federation_version();
        }
        linked_federation_version
    }

    fn get_fields_with_from_context_directive(
        subgraphs: &[Subgraph<Validated>],
    ) -> DirectiveReferencers {
        subgraphs
            .iter()
            .fold(Default::default(), |mut acc, subgraph| {
                if let Ok(Some(directive_name)) = subgraph.from_context_directive_name()
                    && let Ok(referencers) = subgraph
                        .schema()
                        .referencers()
                        .get_directive(&directive_name)
                {
                    acc.extend(referencers);
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
                if let Ok(Some(directive_name)) = subgraph.override_directive_name()
                    && let Ok(referencers) = subgraph
                        .schema()
                        .referencers()
                        .get_directive(&directive_name)
                {
                    acc.extend(referencers);
                }
                acc
            })
    }

    fn prepare_supergraph(&mut self) -> Result<(), FederationError> {
        // Add the @link specification to the merged schema
        self.link_spec_definition
            .add_to_schema(&mut self.merged, None)?;

        // Apply the @join specification to the schema
        self.link_spec_definition.apply_feature_to_schema(
            &mut self.merged,
            self.join_spec_definition,
            None,
            self.join_spec_definition.purpose(),
            None, // imports
        )?;

        let directives_merge_info = self.collect_core_directives_to_compose()?;

        self.validate_and_maybe_add_specs(&directives_merge_info)?;

        // Populate the graph enum with subgraph information and store the mapping
        self.subgraph_names_to_join_spec_name = self
            .join_spec_definition
            .populate_graph_enum(&mut self.merged, &self.subgraphs)?;

        Ok(())
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

    /// Get access to the merged schema
    pub(crate) fn schema(&self) -> &FederationSchema {
        &self.merged
    }

    /// Get access to the error reporter
    pub(crate) fn error_reporter(&self) -> &ErrorReporter {
        &self.error_reporter
    }

    /// Get mutable access to the error reporter
    pub(crate) fn error_reporter_mut(&mut self) -> &mut ErrorReporter {
        &mut self.error_reporter
    }

    /// Get access to the subgraph names
    pub(crate) fn subgraph_names(&self) -> &[String] {
        &self.names
    }

    /// Get access to the enum usages
    pub(crate) fn enum_usages(&self) -> &HashMap<String, EnumTypeUsage> {
        &self.enum_usages
    }

    /// Get mutable access to the enum usages
    pub(crate) fn enum_usages_mut(&mut self) -> &mut HashMap<String, EnumTypeUsage> {
        &mut self.enum_usages
    }

    /// Check if there are any errors
    pub(crate) fn has_errors(&self) -> bool {
        self.error_reporter.has_errors()
    }

    /// Check if there are any hints
    pub(crate) fn has_hints(&self) -> bool {
        self.error_reporter.has_hints()
    }

    /// Get enum usage for a specific enum type
    pub(crate) fn get_enum_usage(&self, enum_name: &str) -> Option<&EnumTypeUsage> {
        self.enum_usages.get(enum_name)
    }

    pub(crate) fn merge(mut self) -> MergeResult {
        // Validate and record usages of @composeDirective
        self.compose_directive_manager
            .validate(&self.subgraphs, &mut self.error_reporter);
        // TODO: JS doesn't include this, but we're bailing here to test error generation while the
        // rest of merge is unimplemented. Once merge can complete without panicking, we can remove
        // this block.
        if self.error_reporter.has_errors() {
            let (errors, hints) = self.error_reporter.into_errors_and_hints();
            return MergeResult {
                supergraph: None,
                errors,
                hints,
            };
        }

        // Add core features to the merged schema
        self.add_core_features();

        // Create empty objects for all types and directive definitions
        self.add_types_shallow();
        self.add_directives_shallow();

        let object_types = self.get_merged_object_type_names();
        let interface_types = self.get_merged_interface_type_names();
        let union_types = self.get_merged_union_type_names();
        let enum_types = self.get_merged_enum_type_names();
        let scalar_types = self.get_merged_scalar_type_names();
        let input_object_types = self.get_merged_input_object_type_names();

        // Merge implements relationships for object and interface types
        for object_type in &object_types {
            self.merge_implements(object_type);
        }

        for interface_type in &interface_types {
            self.merge_implements(interface_type);
        }

        // Merge union types
        for union_type in &union_types {
            self.merge_type(union_type);
        }

        // Merge schema definition (root types)
        self.merge_schema_definition();

        // Merge non-union and non-enum types
        for type_def in &scalar_types {
            self.merge_type(type_def);
        }
        for type_def in &input_object_types {
            self.merge_type(type_def);
        }

        // Merge directive definitions
        self.merge_directive_definitions();

        // Merge enum types last
        for enum_type in &enum_types {
            self.merge_type(enum_type);
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

    fn add_core_features(&mut self) {
        todo!("Implement adding core features to merged schema")
    }

    fn add_types_shallow(&mut self) {
        let mut mismatched_types = HashSet::new();
        let mut types_with_interface_object = HashSet::new();

        for subgraph in &self.subgraphs {
            for pos in subgraph.schema().get_types() {
                if !self.is_merged_type(subgraph, &pos) {
                    continue;
                }

                let mut expects_interface = false;
                if subgraph.is_interface_object_type(&pos) {
                    expects_interface = true;
                    types_with_interface_object.insert(pos.clone());
                }
                if let Ok(previous) = self.merged.get_type(pos.type_name().clone()) {
                    if expects_interface
                        && !matches!(previous, TypeDefinitionPosition::Interface(_))
                    {
                        mismatched_types.insert(pos.clone());
                    }
                    if !expects_interface && previous != pos {
                        mismatched_types.insert(pos.clone());
                    }
                } else if expects_interface {
                    let itf_pos = InterfaceTypeDefinitionPosition {
                        type_name: pos.type_name().clone(),
                    };
                    itf_pos.pre_insert(&mut self.merged);
                    itf_pos.insert_empty(&mut self.merged);
                } else {
                    pos.pre_insert(&mut self.merged);
                    pos.insert_empty(&mut self.merged);
                }
            }
        }

        for mismatched_type in mismatched_types.iter() {
            self.report_mismatched_type_definitions(mismatched_type, &types_with_interface_object);
        }

        // Most invalid use of @interfaceObject are reported as a mismatch above, but one exception is the
        // case where a type is used only with @interfaceObject, but there is no corresponding interface
        // definition in any subgraph.
        for type_ in types_with_interface_object.iter() {
            if mismatched_types.contains(type_) {
                continue;
            }

            let mut found_interface = false;
            let mut subgraphs_with_type = HashSet::new();
            for subgraph in &self.subgraphs {
                let type_in_subgraph = subgraph.schema().get_type(type_.type_name().clone());
                if matches!(type_in_subgraph, Ok(TypeDefinitionPosition::Interface(_))) {
                    found_interface = true;
                    break;
                }
                if type_in_subgraph.is_ok() {
                    subgraphs_with_type.insert(subgraph.name.clone());
                }
            }

            // Note that there is meaningful way in which the supergraph could work in this situation, expect maybe if
            // the type is unused, because validation composition would complain it cannot find the `__typename` in path
            // leading to that type. But the error here is a bit more "direct"/user friendly than what post-merging
            // validation would return, so we make this a hard error, not just a warning.
            if !found_interface {
                self.error_reporter.add_error(CompositionError::InterfaceObjectUsageError { message: format!(
                    "Type \"{}\" is declared with @interfaceObject in all the subgraphs in which it is defined (it is defined in {} but should be defined as an interface in at least one subgraph)",
                    type_.type_name(),
                    human_readable_subgraph_names(subgraphs_with_type.iter())
                ) });
            }
        }
    }

    fn is_merged_type(
        &self,
        subgraph: &Subgraph<Validated>,
        type_: &TypeDefinitionPosition,
    ) -> bool {
        if type_.is_introspection_type() || FEDERATION_OPERATION_TYPES.contains(type_.type_name()) {
            return false;
        }

        let type_feature = subgraph
            .schema()
            .metadata()
            .and_then(|links| links.source_link_of_type(type_.type_name()));
        let exists_and_is_excluded = type_feature
            .is_some_and(|link| NON_MERGED_CORE_FEATURES.contains(&link.link.url.identity));
        !exists_and_is_excluded
    }

    fn report_mismatched_type_definitions(
        &mut self,
        mismatched_type: &TypeDefinitionPosition,
        types_with_interface_object: &HashSet<TypeDefinitionPosition>,
    ) {
        let sources = self
            .subgraphs
            .iter()
            .enumerate()
            .map(|(idx, sg)| {
                (
                    idx,
                    sg.schema()
                        .get_type(mismatched_type.type_name().clone())
                        .ok(),
                )
            })
            .collect();
        let type_kind_to_string = |type_def: &TypeDefinitionPosition, _| {
            let type_kind_description = if types_with_interface_object.contains(type_def) {
                "Interface Object Type (Object Type with @interfaceObject)".to_string()
            } else {
                type_def.kind().replace("Type", " Type")
            };
            Some(type_kind_description)
        };
        // TODO: Second type param is supposed to be representation of AST nodes
        self.error_reporter
            .report_mismatch_error::<TypeDefinitionPosition, ()>(
                CompositionError::TypeKindMismatch {
                    message: format!(
                        "Type \"{}\" has mismatched kind: it is defined as ",
                        mismatched_type.type_name()
                    ),
                },
                mismatched_type,
                &sources,
                type_kind_to_string,
            );
    }

    fn add_directives_shallow(&mut self) -> Result<(), FederationError> {
        for subgraph in self.subgraphs.iter() {
            for (name, definition) in subgraph.schema().schema().directive_definitions.iter() {
                if self.merged.get_directive_definition(name).is_none()
                    && self.is_merged_directive_definition(&subgraph.name, definition)
                {
                    let pos = DirectiveDefinitionPosition {
                        directive_name: name.clone(),
                    };
                    pos.pre_insert(&mut self.merged)?;
                    pos.insert(&mut self.merged, definition.clone())?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::merger) fn is_merged_directive(
        &self,
        subgraph_name: &str,
        directive: &Directive,
    ) -> bool {
        if self
            .compose_directive_manager
            .should_compose_directive(subgraph_name, &directive.name)
        {
            return true;
        }

        self.merged_federation_directive_names
            .contains(directive.name.as_str())
            || BUILT_IN_DIRECTIVES.contains(&directive.name.as_str())
    }

    pub(in crate::merger) fn is_merged_directive_definition(
        &self,
        subgraph_name: &str,
        definition: &DirectiveDefinition,
    ) -> bool {
        if self
            .compose_directive_manager
            .should_compose_directive(subgraph_name, &definition.name)
        {
            return true;
        }

        !BUILT_IN_DIRECTIVES.contains(&definition.name.as_str())
            && definition
                .locations
                .iter()
                .any(|loc| loc.is_executable_location())
    }

    /// Gets the names of all Object types that should be merged. This excludes types that are part
    /// of the link or join specs. Assumes all candidate types have at least been shallow-copied to
    /// the supergraph schema already.
    fn get_merged_object_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .object_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    /// Gets the names of all Interface types that should be merged. This excludes types that are
    /// part of the link or join specs. Assumes all candidate types have at least been
    /// shallow-copied to the supergraph schema already.
    fn get_merged_interface_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .interface_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    /// Gets the names of all Union types that should be merged. This excludes types that are part
    /// of the link or join specs. Assumes all candidate types have at least been shallow-copied to
    /// the supergraph schema already.
    fn get_merged_union_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .union_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    /// Gets the names of all InputObject types that should be merged. This excludes types that are
    /// part of the link or join specs. Assumes all candidate types have at least been shallow-copied
    /// to the supergraph schema already.
    fn get_merged_input_object_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .input_object_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    /// Gets the names of all Scalar types that should be merged. This excludes types that are part
    /// of the link or join specs. Assumes all candidate types have at least been shallow-copied to
    /// the supergraph schema already.
    fn get_merged_scalar_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .scalar_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    /// Gets the names of all Enum types that should be merged. This excludes types that are part
    /// of the link or join specs. Assumes all candidate types have at least been shallow-copied to
    /// the supergraph schema already.
    fn get_merged_enum_type_names(&self) -> Vec<Name> {
        self.merged
            .referencers()
            .enum_types
            .keys()
            .filter(|n| self.should_merge_type(n))
            .cloned()
            .collect_vec()
    }

    fn should_merge_type(&self, name: &Name) -> bool {
        !self
            .link_spec_definition
            .is_spec_type_name(&self.merged, name)
            .unwrap_or(false)
            && !self
                .join_spec_definition
                .is_spec_type_name(&self.merged, name)
                .unwrap_or(false)
    }

    fn merge_implements(&mut self, type_def: &Name) -> Result<(), FederationError> {
        let dest = self.merged.get_type(type_def.clone())?;
        let dest: ObjectOrInterfaceTypeDefinitionPosition = dest.try_into().map_err(|_| {
            internal_error!(
                "Expected type {} to be an Object or Interface type, but it is not",
                type_def
            )
        })?;
        let mut implemented = IndexSet::new();
        for (idx, subgraph) in self.subgraphs.iter().enumerate() {
            let Some(ty) = subgraph.schema().schema().types.get(type_def) else {
                continue;
            };
            let graph_name = self.join_spec_name(idx)?.clone();
            match ty {
                ExtendedType::Object(obj) => {
                    for implemented_itf in obj.implements_interfaces.iter() {
                        implemented.insert(implemented_itf.clone());
                        let join_implements = self
                            .join_spec_definition
                            .implements_directive(graph_name.clone(), implemented_itf);
                        dest.insert_directive(&mut self.merged, Component::new(join_implements))?;
                    }
                }
                ExtendedType::Interface(itf) => {
                    for implemented_itf in itf.implements_interfaces.iter() {
                        implemented.insert(implemented_itf.clone());
                        let join_implements = self
                            .join_spec_definition
                            .implements_directive(graph_name.clone(), implemented_itf);
                        dest.insert_directive(&mut self.merged, Component::new(join_implements))?;
                    }
                }
                _ => continue,
            }
        }
        for implemented_itf in implemented {
            dest.insert_implements_interface(&mut self.merged, implemented_itf)?;
        }
        Ok(())
    }

    pub(crate) fn merge_object(
        &mut self,
        obj: ObjectTypeDefinitionPosition,
    ) -> Result<(), FederationError> {
        let is_entity = self.hint_on_inconsistent_entity(&obj)?;
        let is_value_type = !is_entity && self.merged.is_root_type(&obj.type_name);
        let is_subscription = self.merged.is_subscription_root_type(&obj.type_name);

        let added = self.add_fields_shallow(&obj)?;
        if added.is_empty() {
            obj.remove(&mut self.merged)?;
        } else {
            for (field, subgraph_fields) in added {
                if is_value_type {
                    self.hint_on_inconsistent_value_type_field(
                        &subgraph_fields,
                        &ObjectOrInterfaceTypeDefinitionPosition::Object(obj.clone()),
                        &field,
                    );
                }
                let merge_context = self.validate_override(&subgraph_fields, &field)?;

                if is_subscription {
                    self.validate_subscription_field(&subgraph_fields, &field)?;
                }

                let directive_target_pos = DirectiveTargetPosition::try_from(field.clone())?;
                let sources = subgraph_fields
                    .iter()
                    .map(|(idx, _)| (*idx, Some(directive_target_pos.clone())))
                    .collect::<IndexMap<usize, Option<DirectiveTargetPosition>>>();
                self.merge_field(&sources, &directive_target_pos, &merge_context)?;
                self.validate_field_sharing(&subgraph_fields, &field, &merge_context)?;
            }
        }
        Ok(())
    }

    fn add_fields_shallow(
        &mut self,
        obj: &ObjectTypeDefinitionPosition,
    ) -> Result<HashMap<FieldDefinitionPosition, Sources<()>>, FederationError> {
        todo!("Implement add_fields_shallow")
    }

    fn validate_override(
        &self,
        sources: &Sources<()>,
        dest: &FieldDefinitionPosition,
    ) -> Result<FieldMergeContext, FederationError> {
        todo!("Implement validate_override")
    }

    fn validate_subscription_field(
        &mut self,
        sources: &Sources<()>,
        dest: &FieldDefinitionPosition,
    ) -> Result<(), FederationError> {
        // no subgraph marks field as @shareable
        let mut fields_with_shareable: Sources<Node<FieldDefinition>> = Default::default();
        for (idx, unit) in sources.iter() {
            if unit.is_some() {
                let subgraph = &self.subgraphs[*idx];
                let shareable_directive_name = &subgraph
                    .metadata()
                    .federation_spec_definition()
                    .shareable_directive_definition(subgraph.schema())?
                    .name;
                if dest.has_applied_directive(subgraph.schema(), shareable_directive_name) {
                    let field = dest.get(subgraph.schema().schema())?;
                    fields_with_shareable.insert(*idx, Some(field.node.clone()));
                }
            }
        }
        if !fields_with_shareable.is_empty() {
            self.error_reporter
                .add_error(CompositionError::InvalidFieldSharing {
                    message:
                        "Fields on root level subscription object cannot be marked as shareable"
                            .to_string(),
                    locations: self.source_locations(&fields_with_shareable),
                });
        }
        Ok(())
    }

    fn are_all_fields_external(
        &self,
        idx: usize,
        ty: &ObjectOrInterfaceTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        Ok(ty.fields(self.merged.schema())?.all(|field| {
            self.subgraphs[idx]
                .metadata()
                .external_metadata()
                .is_external(&FieldDefinitionPosition::from(field.clone()))
        }))
    }

    fn hint_on_inconsistent_value_type_field(
        &mut self,
        sources: &Sources<()>,
        dest: &ObjectOrInterfaceTypeDefinitionPosition,
        field: &FieldDefinitionPosition,
    ) -> Result<(), FederationError> {
        let (hint_id, type_description) = match field {
            FieldDefinitionPosition::Object(field) => (
                HintCode::InconsistentObjectValueTypeField,
                "non-entity object",
            ),
            FieldDefinitionPosition::Interface(field) => (
                HintCode::InconsistentInterfaceValueTypeField,
                "non-entity interface",
            ),
            _ => bail!("Expected field to be an Object or Interface field, but it is not"),
        };
        for (idx, unit) in sources.iter() {
            if unit.is_some() {
                let subgraph = &self.subgraphs[*idx];
                let field_pos = dest.field(field.field_name().clone());
                let field = field_pos.try_get(self.merged.schema());
                if field.is_none() && !self.are_all_fields_external(*idx, dest)? {
                    // transform sources to ExtendedType sources
                    let printable_sources = sources
                        .iter()
                        .map(|(idx, pos)| match pos {
                            None => (*idx, None),
                            Some(_) => {
                                let extended_type = subgraph
                                    .schema()
                                    .schema()
                                    .types
                                    .get(dest.type_name())
                                    .cloned();
                                (*idx, extended_type)
                            }
                        })
                        .collect::<IndexMap<usize, Option<ExtendedType>>>();
                    let dest_in_supergraph = match dest {
                        ObjectOrInterfaceTypeDefinitionPosition::Object(obj) => {
                            ExtendedType::Object(obj.get(self.merged.schema())?.clone())
                        }
                        ObjectOrInterfaceTypeDefinitionPosition::Interface(itf) => {
                            ExtendedType::Interface(itf.get(self.merged.schema())?.clone())
                        }
                    };
                    self.error_reporter.report_mismatch_hint::<ExtendedType, ()>(
                        hint_id.clone(),
                        format!("Field \"{}.{}\" of {} type \"{}\" is defined in some but not all subgraphs that define \"{}\"",
                            dest.type_name(),
                            field_pos.field_name(),
                            type_description,
                            dest.type_name(),
                            dest.type_name(),
                        ),
                        &dest_in_supergraph,
                        &printable_sources,
                        |ty, _| match ty {
                            ExtendedType::Object(obj) => {
                                if obj.fields.contains_key(field_pos.field_name()) {
                                    Some("yes".to_string())
                                } else {
                                    Some("no".to_string())
                                }
                            },
                            ExtendedType::Interface(itf) => {
                                if itf.fields.contains_key(field_pos.field_name()) {
                                    Some("yes".to_string())
                                } else {
                                    Some("no".to_string())
                                }
                            },
                            _ => Some("no".to_string()),
                        },
                        |_, subgraphs| format!("\"{}.{}\" is defined in {}", field_pos.type_name(), field_pos.field_name(), subgraphs.unwrap_or_default()),
                        |_, subgraphs| format!(" but not in {}", subgraphs),
                        None::<fn(Option<&ExtendedType>) -> bool>,
                        false,
                        false,
                    );
                }
            }
        }
        Ok(())
    }

    fn hint_on_inconsistent_entity(
        &mut self,
        obj: &ObjectTypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let mut source_as_entity = Vec::new();
        let mut source_as_non_entity = Vec::new();

        let mut sources: Sources<usize> = Default::default();
        for (idx, subgraph) in self.subgraphs.iter().enumerate() {
            let Some(key_directive_name) = subgraph.key_directive_name()? else {
                continue;
            };
            if obj.try_get(subgraph.schema().schema()).is_some() {
                sources.insert(idx, Some(idx));
                if obj.has_applied_directive(subgraph.schema(), &key_directive_name) {
                    source_as_entity.push(idx);
                } else {
                    source_as_non_entity.push(idx);
                }
            }
        }
        let supergraph = 0;
        if !source_as_entity.is_empty() && !source_as_non_entity.is_empty() {
            self.error_reporter.report_mismatch_hint::<usize, ()>(
                        HintCode::InconsistentEntity,
                        format!("Type \"{}\" is declared as an entity (has a @key applied) in some but all defining subgraphs: ",
                            &obj.type_name,
                        ),
                        &supergraph,
                        &sources,
                        |idx, _| if source_as_entity.contains(idx) { Some("yes".to_string()) } else { Some("no".to_string()) },
                        |_, subgraphs| format!("it has no @key in {}", subgraphs.unwrap_or_default()),
                        |_, subgraphs| format!(" but has some @key in {}", subgraphs),
                        None::<fn(Option<&usize>) -> bool>,
                        false,
                        false,
                    );
        }
        Ok(!source_as_entity.is_empty())
    }

    pub(crate) fn merge_interface(&mut self, itf: InterfaceTypeDefinitionPosition) {
        todo!("Implement merge_interface")
    }

    pub(crate) fn merge_input_object(&mut self, io: InputObjectTypeDefinitionPosition) {
        todo!("Implement merge_input_object")
    }

    fn merge_schema_definition(&mut self) {
        let sources: Sources<SchemaDefinitionPosition> = self
            .subgraphs
            .iter()
            .enumerate()
            .map(|(idx, subgraph)| (idx, Some(SchemaDefinitionPosition {})))
            .collect();
        let dest = SchemaDefinitionPosition {};

        self.merge_description(&sources, &dest);
        self.record_applied_directives_to_merge(&sources, &dest);
        self.add_join_directive_directives(&sources, &dest);
    }

    fn merge_directive_definitions(&mut self) {
        todo!("Implement directive definition merging")
    }

    fn validate_query_root(&mut self) {
        todo!("Implement query root validation")
    }

    pub(in crate::merger) fn directive_applications_with_transformed_arguments(
        pos: &DirectiveTargetPosition,
        merge_info: &MergedDirectiveInfo,
        subgraph: &Subgraph<Validated>,
    ) -> Vec<Directive> {
        let mut applications = Vec::new();
        if let Some(arg_transform) = &merge_info.static_argument_transform {
            for application in
                pos.get_applied_directives(subgraph.schema(), &merge_info.definition.name)
            {
                let mut transformed_application = Directive::new(application.name.clone());
                let indexed_args: IndexMap<Name, Value> = application
                    .arguments
                    .iter()
                    .map(|a| (a.name.clone(), a.value.as_ref().clone()))
                    .collect();
                transformed_application.arguments = arg_transform(subgraph, indexed_args)
                    .into_iter()
                    .map(|(name, value)| {
                        Node::new(Argument {
                            name,
                            value: Node::new(value),
                        })
                    })
                    .collect();
                applications.push(transformed_application);
            }
        }
        applications
    }

    fn add_missing_interface_object_fields_to_implementations(&mut self) {
        todo!("Implement adding missing interface object fields to implementations")
    }

    fn post_merge_validations(&mut self) {
        todo!("Implement post-merge validations")
    }

    /// Core type merging logic for GraphQL Federation composition.
    ///
    /// Merges type references from multiple subgraphs following Federation variance rules:
    /// - For output positions: uses the most general (supertype) when types are compatible
    /// - For input positions: uses the most specific (subtype) when types are compatible
    /// - Reports errors for incompatible types, hints for compatible but inconsistent types
    /// - Tracks enum usage for validation purposes
    pub(crate) fn merge_type_reference<TElement>(
        &mut self,
        sources: &Sources<Type>,
        dest: &mut TElement,
        is_input_position: bool,
        parent_type_name: &str, // We need this for the coordinate as FieldDefinition lack parent context
    ) -> Result<bool, FederationError>
    where
        TElement: SchemaElementWithType,
    {
        // Validate sources
        if sources.is_empty() {
            self.error_reporter_mut()
                .add_error(CompositionError::InternalError {
                    message: format!(
                        "No type sources provided for merging {}",
                        dest.coordinate(parent_type_name)
                    ),
                });
            return Ok(false);
        }

        // Build iterator over the non-None source types
        let mut iter = sources.values().filter_map(Option::as_ref);
        let mut has_subtypes = false;
        let mut has_incompatible = false;

        // Grab the first type (if any) to initialise comparison
        let Some(mut typ) = iter.next() else {
            // No concrete type found in any subgraph — this should not normally happen
            let error = CompositionError::InternalError {
                message: format!(
                    "No type sources provided for {} across subgraphs",
                    dest.coordinate(parent_type_name)
                ),
            };
            self.error_reporter_mut().add_error(error);
            return Ok(false);
        };

        // Determine the merged type following GraphQL Federation variance rules
        for source_type in iter {
            if Self::same_type(typ, source_type) {
                // Types are identical
                continue;
            } else if let Ok(true) = self.is_strict_subtype(source_type, typ) {
                // current typ is a subtype of source_type (source_type is more general)
                has_subtypes = true;
                if is_input_position {
                    // For input: upgrade to the supertype
                    typ = source_type;
                }
            } else if let Ok(true) = self.is_strict_subtype(typ, source_type) {
                // source_type is a subtype of current typ (current typ is more general)
                has_subtypes = true;
                if !is_input_position {
                    // For output: keep the supertype; for input: adopt the subtype
                    typ = source_type;
                }
            } else {
                has_incompatible = true;
            }
        }

        // Copy the type reference to the destination schema
        let copied_type = self.copy_type_reference(typ)?;

        dest.set_type(copied_type);

        let ast_node = dest.enum_example_ast();
        self.track_enum_usage(
            typ,
            dest.coordinate(parent_type_name),
            ast_node,
            is_input_position,
        );

        let element_kind = if is_input_position {
            "argument"
        } else {
            "field"
        };

        if has_incompatible {
            // Report incompatible type error
            let error_code_str = if is_input_position {
                "ARGUMENT_TYPE_MISMATCH"
            } else {
                "FIELD_TYPE_MISMATCH"
            };

            let error = CompositionError::InternalError {
                message: format!(
                    "Type of {} \"{}\" is incompatible across subgraphs",
                    element_kind,
                    dest.coordinate(parent_type_name)
                ),
            };

            self.error_reporter_mut().report_mismatch_error::<Type, ()>(
                error,
                typ,
                sources,
                |typ, _is_supergraph| Some(format!("type \"{typ}\"")),
            );

            Ok(false)
        } else if has_subtypes {
            // Report compatibility hint for subtype relationships
            let hint_code = if is_input_position {
                HintCode::InconsistentButCompatibleArgumentType
            } else {
                HintCode::InconsistentButCompatibleFieldType
            };

            let type_class = if is_input_position {
                "supertype"
            } else {
                "subtypes"
            };

            self.error_reporter_mut().report_mismatch_hint::<Type, ()>(
                hint_code,
                format!(
                    "Type of {} \"{}\" is inconsistent but compatible across subgraphs:",
                    element_kind,
                    dest.coordinate(parent_type_name)
                ),
                typ,
                sources,
                |typ, _is_supergraph| Some(format!("type \"{typ}\"")),
                |elt, subgraphs| {
                    format!(
                        "will use type \"{}\" (from {}) in supergraph but \"{}\" has ",
                        elt,
                        subgraphs.unwrap_or_else(|| "undefined".to_string()),
                        dest.coordinate(parent_type_name)
                    )
                },
                |elt, subgraphs| format!("{type_class} \"{elt}\" in {subgraphs}"),
                None::<fn(Option<&Type>) -> bool>,
                false,
                false,
            );

            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn track_enum_usage(
        &mut self,
        typ: &Type,
        element_name: String,
        element_ast: Option<EnumExampleAst>,
        is_input_position: bool,
    ) {
        // Get the base type (unwrap nullability and list wrappers)
        let base_type_name = typ.inner_named_type();

        // Check if it's an enum type
        if let Some(&ExtendedType::Enum(_)) = self.schema().schema().types.get(base_type_name) {
            let default_example = || EnumExample {
                coordinate: element_name,
                element_ast: element_ast.clone(),
            };

            // Compute the new usage directly based on existing record and current position
            let new_usage = match self.enum_usages().get(base_type_name.as_str()) {
                Some(EnumTypeUsage::Input { input_example }) if !is_input_position => {
                    EnumTypeUsage::Both {
                        input_example: input_example.clone(),
                        output_example: default_example(),
                    }
                }
                Some(EnumTypeUsage::Input { input_example })
                | Some(EnumTypeUsage::Both { input_example, .. })
                    if is_input_position =>
                {
                    EnumTypeUsage::Input {
                        input_example: input_example.clone(),
                    }
                }
                Some(EnumTypeUsage::Output { output_example }) if is_input_position => {
                    EnumTypeUsage::Both {
                        input_example: default_example(),
                        output_example: output_example.clone(),
                    }
                }
                Some(EnumTypeUsage::Output { output_example })
                | Some(EnumTypeUsage::Both { output_example, .. })
                    if !is_input_position =>
                {
                    EnumTypeUsage::Output {
                        output_example: output_example.clone(),
                    }
                }
                _ if is_input_position => EnumTypeUsage::Input {
                    input_example: default_example(),
                },
                _ => EnumTypeUsage::Output {
                    output_example: default_example(),
                },
            };

            // Store updated usage
            self.enum_usages_mut()
                .insert(base_type_name.to_string(), new_usage);
        }
    }

    fn same_type(dest_type: &Type, source_type: &Type) -> bool {
        match (dest_type, source_type) {
            (Type::Named(n1), Type::Named(n2)) => n1 == n2,
            (Type::NonNullNamed(n1), Type::NonNullNamed(n2)) => n1 == n2,
            (Type::List(inner1), Type::List(inner2)) => Self::same_type(inner1, inner2),
            (Type::NonNullList(inner1), Type::NonNullList(inner2)) => {
                Self::same_type(inner1, inner2)
            }
            _ => false,
        }
    }

    pub(in crate::merger) fn is_strict_subtype(
        &self,
        potential_supertype: &Type,
        potential_subtype: &Type,
    ) -> Result<bool, FederationError> {
        // Hardcoded subtyping rules based on the default configuration:
        // - Direct: Interface/union subtyping relationships
        // - NonNullableDowngrade: NonNull T is subtype of T
        // - ListPropagation: [T] is subtype of [U] if T is subtype of U
        // - NonNullablePropagation: NonNull T is subtype of NonNull U if T is subtype of U
        // - ListUpgrade is NOT supported (was excluded by default)

        match (potential_subtype, potential_supertype) {
            // -------- List & NonNullList --------
            // ListPropagation: [T] is subtype of [U] if T is subtype of U
            (Type::List(inner_sub), Type::List(inner_super)) => {
                self.is_strict_subtype(inner_super, inner_sub)
            }
            // NonNullablePropagation and NonNullableDowngrade
            (Type::NonNullList(inner_sub), Type::NonNullList(inner_super))
            | (Type::NonNullList(inner_sub), Type::List(inner_super)) => {
                self.is_strict_subtype(inner_super, inner_sub)
            }

            // Anything else with list on the left is not a strict subtype
            (Type::List(_), _) | (Type::NonNullList(_), _) => Ok(false),

            // -------- Named & NonNullNamed --------
            // Same named type => not strict subtype
            (Type::Named(a), Type::Named(b)) | (Type::Named(a), Type::NonNullNamed(b))
                if a == b =>
            {
                Ok(false)
            }
            (Type::NonNullNamed(a), Type::NonNullNamed(b)) if a == b => Ok(false),

            // NonNull downgrade: T! ⊑ T
            (Type::NonNullNamed(sub), Type::Named(super_)) if sub == super_ => Ok(true),

            // Interface/Union relationships (includes downgrade handled above)
            (Type::Named(sub), Type::Named(super_))
            | (Type::Named(sub), Type::NonNullNamed(super_))
            | (Type::NonNullNamed(sub), Type::Named(super_))
            | (Type::NonNullNamed(sub), Type::NonNullNamed(super_)) => {
                self.is_named_type_subtype(super_, sub)
            }

            // ListUpgrade not supported; any other combination is not strict
            _ => Ok(false),
        }
    }

    fn is_named_type_subtype(
        &self,
        potential_supertype: &NamedType,
        potential_subtype: &NamedType,
    ) -> Result<bool, FederationError> {
        let Some(subtype_def) = self.schema().schema().types.get(potential_subtype) else {
            bail!("Cannot find type '{}' in schema", potential_subtype);
        };

        let Some(supertype_def) = self.schema().schema().types.get(potential_supertype) else {
            bail!("Cannot find type '{}' in schema", potential_supertype);
        };

        // Direct subtyping relationships (interface/union) are always supported
        match (subtype_def, supertype_def) {
            // Object type implementing an interface
            (ExtendedType::Object(obj), ExtendedType::Interface(_)) => {
                Ok(obj.implements_interfaces.contains(potential_supertype))
            }
            // Interface extending another interface
            (ExtendedType::Interface(sub_intf), ExtendedType::Interface(_)) => {
                Ok(sub_intf.implements_interfaces.contains(potential_supertype))
            }
            // Object type that is a member of a union
            (ExtendedType::Object(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(potential_subtype))
            }
            // Interface that is a member of a union (if supported)
            (ExtendedType::Interface(_), ExtendedType::Union(union_type)) => {
                Ok(union_type.members.contains(potential_subtype))
            }
            _ => Ok(false),
        }
    }

    pub(crate) fn copy_type_reference(
        &mut self,
        source_type: &Type,
    ) -> Result<Type, FederationError> {
        // Check if the type is already defined in the target schema
        let target_schema = self.schema().schema();

        let name = source_type.inner_named_type();
        if !target_schema.types.contains_key(name) {
            self.error_reporter_mut()
                .add_error(CompositionError::InternalError {
                    message: format!("Cannot find type '{name}' in target schema"),
                });
        }

        Ok(source_type.clone())
    }

    pub(in crate::merger) fn merge_description<T>(&mut self, sources: &Sources<T>, dest: &T)
    where
        T: HasDescription + std::fmt::Display,
    {
        let mut descriptions: CountMap<String, usize> = CountMap::new();

        for (idx, source) in sources {
            // Skip if source has no description
            let Some(source_desc) = source
                .as_ref()
                .and_then(|s| s.description(self.subgraphs[*idx].schema()))
            else {
                continue;
            };

            descriptions.insert_or_increment(source_desc.trim().to_string());
        }
        // we don't want to raise a hint if a description is ""
        descriptions.remove(&String::new());

        if !descriptions.is_empty() {
            if let Some((description, _)) = iter_into_single_item(descriptions.iter()) {
                dest.set_description(&mut self.merged, Some(Node::new_str(description)));
            } else {
                // Find the description with the highest count
                if let Some((idx, _)) = descriptions
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, (_, counts))| *counts)
                {
                    // Get the description at the found index
                    if let Some((description, _)) = descriptions.iter().nth(idx) {
                        dest.set_description(&mut self.merged, Some(Node::new_str(description)));
                    }
                }
                // TODO: Currently showing full descriptions in the hint
                // messages, which is probably fine in some cases. However this
                // might get less helpful if the description appears to differ
                // by a very small amount (a space, a single character typo) and
                // even more so the bigger the description is, and we could
                // improve the experience here. For instance, we could print the
                // supergraph description but then show other descriptions as
                // diffs from that (using, say,
                // https://www.npmjs.com/package/diff). And we could even switch
                // between diff/non-diff modes based on the levenshtein
                // distances between the description we found. That said, we
                // should decide if we want to bother here: maybe we can leave
                // it to studio so handle a better experience (as it can more UX
                // wise).
                let coordinate = dest.to_string();
                let name = if !coordinate.is_empty() {
                    "Element {coordinate}"
                } else {
                    "The schema definition"
                };
                self.error_reporter.report_mismatch_hint::<T, ()>(
                    HintCode::InconsistentDescription,
                    format!("{name} has inconsistent descriptions across the subgraphs. "),
                    dest,
                    sources,
                    |elem, _is_supergraph| {
                        elem.description(&self.merged).map(|desc| desc.to_string())
                    },
                    |desc, subgraphs| {
                        format!(
                            "The supergraph will use description (from {}):\n{}",
                            subgraphs.unwrap_or_else(|| "undefined".to_string()),
                            Self::description_string(desc, "  ")
                        )
                    },
                    |desc, subgraphs| {
                        format!(
                            "\nIn {}, the description is:\n{}",
                            subgraphs,
                            Self::description_string(desc, "  ")
                        )
                    },
                    Some(|elem: Option<&T>| {
                        if let Some(el) = elem {
                            el.description(&self.merged).is_none()
                        } else {
                            true
                        }
                    }),
                    false,
                    true,
                );
            }
        }
    }

    pub(in crate::merger) fn description_string(to_indent: &str, indentation: &str) -> String {
        format!(
            "{indentation}\"\"\"\n{indentation}{}\n{indentation}\"\"\"",
            to_indent.replace('\n', &format!("\n{indentation}"))
        )
    }

    /// This method gets called at various points during the merge to allow subgraph directive
    /// applications to be reflected (unapplied) in the supergraph, using the
    /// @join__directive(graphs, name, args) directive.
    pub(in crate::merger) fn add_join_directive_directives<T>(
        &mut self,
        sources: &Sources<T>,
        dest: &T,
    ) -> Result<(), FederationError>
    where
        // If we implemented a `HasDirectives` trait for this bound, we could call that instead
        // of cloning and converting to `DirectiveTargetPosition`.
        T: Clone + TryInto<DirectiveTargetPosition>,
        FederationError: From<<T as TryInto<DirectiveTargetPosition>>::Error>,
    {
        // Joins are grouped by directive name and arguments. So, a directive with the same
        // arguments in multiple subgraphs is merged with a single `@join__directive` that
        // specifies both graphs. If two applications have different arguments, each application
        // gets its own `@join__directive` specifying the different arugments per graph.
        let mut joins_by_directive_name: HashMap<
            Name,
            HashMap<Vec<Node<Argument>>, IndexSet<Name>>,
        > = HashMap::new();
        let mut links_to_persist: Vec<(Url, Directive)> = Vec::new();

        for (idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };
            let graph = self.join_spec_name(*idx)?;
            let schema = self.subgraphs[*idx].schema();
            let Some(link_import_identity_url_map) = schema.metadata() else {
                continue;
            };
            let Ok(Some(link_directive_name)) = self
                .link_spec_definition
                .directive_name_in_schema(schema, &DEFAULT_LINK_NAME)
            else {
                continue;
            };

            let source: DirectiveTargetPosition = source.clone().try_into()?;
            for directive in source.get_all_applied_directives(schema).iter() {
                let mut should_include_as_join_directive = false;

                if directive.name == link_directive_name {
                    if let Ok(link) = Link::from_directive_application(directive) {
                        should_include_as_join_directive =
                            self.should_use_join_directive_for_url(&link.url);

                        if should_include_as_join_directive
                            && SPEC_REGISTRY.get_definition(&link.url).is_some()
                        {
                            links_to_persist.push((link.url.clone(), directive.as_ref().clone()));
                        }
                    }
                } else if let Some(url_for_directive) =
                    link_import_identity_url_map.source_link_of_directive(&directive.name)
                {
                    should_include_as_join_directive =
                        self.should_use_join_directive_for_url(&url_for_directive.link.url);
                }

                if should_include_as_join_directive {
                    let existing_joins = joins_by_directive_name
                        .entry(directive.name.clone())
                        .or_default();
                    let existing_graphs_with_these_arguments = existing_joins
                        .entry(directive.arguments.clone())
                        .or_default();
                    existing_graphs_with_these_arguments.insert(graph.clone());
                }
            }
        }

        let Some(link_directive_name) = self
            .link_spec_definition
            .directive_name_in_schema(&self.merged, &DEFAULT_LINK_NAME)?
        else {
            bail!(
                "Link directive must exist in the supergraph schema in order to apply join directives"
            );
        };

        // When adding links to the supergraph schema, we have to pick a single version (see
        // `Merger::validate_and_maybe_add_specs` for spec selection). For pre-1.0 specs, like the
        // join spec, we generally take the latest known version because they are not necessarily
        // compatible from version to version. This means upgrading composition version will likely
        // change the output supergraph schema. Here, when we encounter a link directive, we
        // preserve the version the subgraph used in a `@join__directive` so the query planner can
        // extract the subgraph schemas with correct links.
        let mut latest_or_highest_link_by_identity: HashMap<Identity, (Url, Directive)> =
            HashMap::new();
        for (url, link_directive) in links_to_persist {
            if let Some((existing_url, existing_directive)) =
                latest_or_highest_link_by_identity.get_mut(&url.identity)
            {
                if url.version > existing_url.version {
                    *existing_url = url;
                    *existing_directive = link_directive;
                }
            } else {
                latest_or_highest_link_by_identity
                    .insert(url.identity.clone(), (url, link_directive));
            }
        }

        let dest: DirectiveTargetPosition = dest.clone().try_into()?;
        for (_, directive) in latest_or_highest_link_by_identity.into_values() {
            // We insert the directive as it was in the subgraph, but with the name of `@link` in
            // the supergraph, in case it was renamed in the subgraph.
            dest.insert_directive(
                &mut self.merged,
                Directive {
                    name: link_directive_name.clone(),
                    arguments: directive.arguments,
                },
            )?;
        }

        let Ok(join_directive_name) = self
            .join_spec_definition
            .directive_name_in_schema(&self.merged, &JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC)
        else {
            // If we got here and have no definition for `@join__directive`, then we're probably
            // operating on a schema that uses join v0.3 or earlier. We don't want to break those
            // schemas, but we also can't insert the directives.
            return Ok(());
        };

        for (name, args_to_graphs_map) in joins_by_directive_name {
            for (args, graphs) in args_to_graphs_map {
                dest.insert_directive(
                    &mut self.merged,
                    self.join_spec_definition
                        .directive_directive(&name, graphs, args),
                )?;
            }
        }

        Ok(())
    }

    fn should_use_join_directive_for_url(&self, url: &Url) -> bool {
        self.join_directive_identities.contains(&url.identity)
    }

    pub(in crate::merger) fn add_arguments_shallow<T>(&mut self, _sources: &Sources<T>, _dest: &T) {
        todo!("Implement add_arguments_shallow")
    }

    pub(in crate::merger) fn merge_default_value<T>(
        &mut self,
        _sources: &Sources<T>,
        _dest: &T,
        kind: &str,
    ) {
        todo!("Implement merge_default_value")
    }

    pub(in crate::merger) fn record_applied_directives_to_merge<T>(
        &mut self,
        _sources: &Sources<T>,
        _dest: &T,
    ) {
        todo!("Implement record_applied_directives_to_merge")
    }

    fn is_inaccessible_directive_in_supergraph(&self, _value: &EnumValueDefinition) -> bool {
        todo!("Implement is_inaccessible_directive_in_supergraph")
    }

    /// Like Iterator::any, but for Sources<T> maps - checks if any source satisfies the predicate
    pub(in crate::merger) fn some_sources<T, F>(sources: &Sources<T>, mut predicate: F) -> bool
    where
        F: FnMut(&Option<T>, usize) -> bool,
    {
        sources.iter().any(|(idx, source)| predicate(source, *idx))
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
        sources: &Sources<Node<T>>,
        accessor: impl Fn(&Option<Node<T>>) -> bool,
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
            locations: self.source_locations(sources),
        };
        self.error_reporter.add_hint(hint);
    }

    /// Merge argument definitions from subgraphs
    pub(in crate::merger) fn merge_argument(
        &mut self,
        _sources: &Sources<Node<InputValueDefinition>>,
        _dest: &Node<InputValueDefinition>,
    ) -> Result<(), FederationError> {
        // TODO: Implement argument merging logic
        // This should merge argument definitions from multiple subgraphs
        // including type validation, default value merging, etc.
        Ok(())
    }

    pub(crate) fn source_locations<T>(&self, sources: &Sources<Node<T>>) -> Vec<SubgraphLocation> {
        let mut result = Vec::new();
        for (subgraph_id, node) in sources {
            let Some(node) = node else {
                continue; // Skip if the node is None
            };
            let Some(subgraph) = self.subgraphs.get(*subgraph_id) else {
                // Skip if the subgraph is not found
                // Note: This is unexpected in production, but it happens in unit tests.
                continue;
            };
            let locations = subgraph
                .schema()
                .node_locations(node)
                .map(|loc| SubgraphLocation {
                    subgraph: subgraph.name.clone(),
                    range: loc,
                });
            result.extend(locations);
        }
        result
    }

    pub(crate) fn subgraph_sources(&self) -> Sources<Subgraph<Validated>> {
        self.subgraphs
            .iter()
            .enumerate()
            .map(|(idx, subgraph)| (idx, Some(subgraph.clone())))
            .collect()
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

/// Map over sources, applying a function to each element
/// TODO: Consider moving this into a trait or Sources
pub(in crate::merger) fn map_sources<T, U, F>(sources: &Sources<T>, f: F) -> Sources<U>
where
    F: Fn(&Option<T>) -> Option<U>,
{
    sources
        .iter()
        .map(|(idx, source)| (*idx, f(source)))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use apollo_compiler::Name;
    use apollo_compiler::Node;
    use apollo_compiler::ast::FieldDefinition;
    use apollo_compiler::ast::InputValueDefinition;
    use apollo_compiler::schema::ComponentName;
    use apollo_compiler::schema::EnumType;
    use apollo_compiler::schema::ExtendedType;
    use apollo_compiler::schema::InterfaceType;
    use apollo_compiler::schema::ObjectType;
    use apollo_compiler::schema::UnionType;

    use super::*;

    /// Test helper struct for type merging tests
    /// In production, this trait is implemented by real schema elements like FieldDefinition and InputValueDefinition
    #[derive(Debug, Clone)]
    pub(crate) struct TestSchemaElement {
        pub(crate) coordinate: String,
        pub(crate) typ: Option<Type>,
    }

    impl SchemaElementWithType for TestSchemaElement {
        fn coordinate(&self, parent_name: &str) -> String {
            format!("{}.{}", parent_name, self.coordinate)
        }

        fn set_type(&mut self, typ: Type) {
            self.typ = Some(typ);
        }
        fn enum_example_ast(&self) -> Option<EnumExampleAst> {
            Some(EnumExampleAst::Field(Node::new(FieldDefinition {
                name: Name::new("dummy").unwrap(),
                description: None,
                arguments: vec![],
                directives: Default::default(),
                ty: Type::Named(Name::new("String").unwrap()),
            })))
        }
    }

    fn create_test_schema() -> Schema {
        let mut schema = Schema::new();

        // Add interface I
        let interface_type = InterfaceType {
            description: None,
            name: Name::new("I").unwrap(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        };
        schema.types.insert(
            Name::new("I").unwrap(),
            ExtendedType::Interface(Node::new(interface_type)),
        );

        // Add object type A implementing I
        let mut object_type = ObjectType {
            description: None,
            name: Name::new("A").unwrap(),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: Default::default(),
        };
        object_type
            .implements_interfaces
            .insert(ComponentName::from(Name::new("I").unwrap()));
        schema.types.insert(
            Name::new("A").unwrap(),
            ExtendedType::Object(Node::new(object_type)),
        );

        // Add union U with member A
        let mut union_type = UnionType {
            description: None,
            name: Name::new("U").unwrap(),
            directives: Default::default(),
            members: Default::default(),
        };
        union_type
            .members
            .insert(ComponentName::from(Name::new("A").unwrap()));
        schema.types.insert(
            Name::new("U").unwrap(),
            ExtendedType::Union(Node::new(union_type)),
        );

        // Add enum Status for enum usage tracking tests
        let enum_type = EnumType {
            description: None,
            name: Name::new("Status").unwrap(),
            directives: Default::default(),
            values: Default::default(),
        };
        schema.types.insert(
            Name::new("Status").unwrap(),
            ExtendedType::Enum(Node::new(enum_type)),
        );

        schema
    }

    fn create_test_merger() -> Result<Merger, FederationError> {
        crate::merger::merge_enum::tests::create_test_merger()
    }

    #[test]
    fn same_types() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("String").unwrap())));

        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );

        // Check that there are no errors or hints
        assert!(result.is_ok());
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn nullable_vs_non_nullable() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::NonNullNamed(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("String").unwrap())));

        // For output types, should use the more general type (nullable)
        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );
        // Check that there are no errors but there might be hints
        assert!(result.is_ok());
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);

        // Create a new merger for the next test since we can't clear the reporter
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // For input types, should use the more specific type (non-nullable)
        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testArg".to_string(),
                typ: None,
            },
            true,
            Name::new("Parent").unwrap().as_str(),
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn interface_subtype() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("I").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("A").unwrap())));

        // For output types, should use the more general type (interface)
        let result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );
        // Check that there are no errors but there might be hints
        assert!(result.is_ok());
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);

        // For input types, should use the more specific type (implementing type)
        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testArg".to_string(),
                typ: None,
            },
            true,
            Name::new("Parent").unwrap().as_str(),
        );
        // Check that there are no errors but there might be hints
        assert!(!merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn incompatible_types() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Int").unwrap())));

        let _result = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "testField".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );
        // Check that there are errors for incompatible types
        assert!(merger.has_errors());
        assert_eq!(merger.enum_usages().len(), 0);
    }

    #[test]
    fn enum_usage_tracking() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test enum usage in output position
        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "user_status".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );

        // Test enum usage in input position
        let mut arg_sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        arg_sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        arg_sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

        let _ = merger.merge_type_reference(
            &arg_sources,
            &mut TestSchemaElement {
                coordinate: "status_filter".to_string(),
                typ: None,
            },
            true,
            Name::new("Parent").unwrap().as_str(),
        );

        // Verify enum usage tracking
        let enum_usage = merger.get_enum_usage("Status");
        assert!(enum_usage.is_some());

        let usage = enum_usage.unwrap();
        match usage {
            EnumTypeUsage::Both {
                input_example,
                output_example,
            } => {
                assert_eq!(input_example.coordinate, "Parent.status_filter");
                assert_eq!(output_example.coordinate, "Parent.user_status");
            }
            _ => panic!("Expected Both usage, got {usage:?}"),
        }
    }

    #[test]
    fn enum_usage_output_only() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Track enum in output position only
        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "status_out".to_string(),
                typ: None,
            },
            false,
            Name::new("Parent").unwrap().as_str(),
        );

        let usage = merger.get_enum_usage("Status").expect("usage");
        match usage {
            EnumTypeUsage::Output { output_example } => {
                assert_eq!(output_example.coordinate, "Parent.status_out");
            }
            _ => panic!("Expected Output usage"),
        }
    }

    #[test]
    fn enum_usage_input_only() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Track enum in input position only
        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Status").unwrap())));
        sources.insert(1, Some(Type::Named(Name::new("Status").unwrap())));

        let _ = merger.merge_type_reference(
            &sources,
            &mut TestSchemaElement {
                coordinate: "status_in".to_string(),
                typ: None,
            },
            true,
            Name::new("Parent").unwrap().as_str(),
        );

        let usage = merger.get_enum_usage("Status").expect("usage");
        match usage {
            EnumTypeUsage::Input { input_example } => {
                assert_eq!(input_example.coordinate, "Parent.status_in");
            }
            _ => panic!("Expected Input usage"),
        }
    }

    #[test]
    fn empty_sources_reports_error() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Test with empty sources
        let sources: Sources<Type> = IndexMap::default();
        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };

        let result = merger.merge_type_reference(
            &sources,
            &mut element,
            false,
            Name::new("Parent").unwrap().as_str(),
        );

        // The implementation returns Ok(false) but adds an error to the error reporter
        match result {
            Ok(false) => {} // Expected
            Ok(true) => panic!("Expected Ok(false), got Ok(true)"),
            Err(e) => panic!("Expected Ok(false), got Err: {e:?}"),
        }
        assert!(
            merger.has_errors(),
            "Expected an error to be reported for empty sources"
        );
    }

    #[test]
    fn sources_with_no_defined_types_reports_error() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        let sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        // both entries None by default

        let mut element = TestSchemaElement {
            coordinate: "f".into(),
            typ: None,
        };

        let result = merger.merge_type_reference(
            &sources,
            &mut element,
            false,
            Name::new("Parent").unwrap().as_str(),
        );

        // The implementation skips None sources, finds no result_type,
        // then returns Ok(false) but adds an error to the error reporter
        match result {
            Ok(false) => {} // Expected
            Ok(true) => panic!("Expected Ok(false), got Ok(true)"),
            Err(e) => panic!("Expected Ok(false), got Err: {e:?}"),
        }
        assert!(
            merger.has_errors(),
            "Expected an error to be reported when no sources have types defined"
        );
    }

    #[test]
    fn merge_with_field_definition_element() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Prepare a field definition in the schema
        let mut field_def = FieldDefinition {
            name: Name::new("field").unwrap(),
            description: None,
            arguments: vec![],
            directives: Default::default(),
            ty: Type::Named(Name::new("String").unwrap()),
        };
        let mut sources: Sources<Type> = (0..1).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("String").unwrap())));

        // Call merge_type_reference on a FieldDefinition (TElement = FieldDefinition)
        let res = merger.merge_type_reference(
            &sources,
            &mut field_def,
            false,
            Name::new("Parent").unwrap().as_str(),
        );
        assert!(
            res.is_ok(),
            "Merging identical types on a FieldDefinition should return true"
        );
        assert_eq!(
            match field_def.ty.clone() {
                Type::Named(n) => n.to_string(),
                _ => String::new(),
            },
            "String"
        );
    }

    #[test]
    fn merge_with_input_value_definition_element() {
        let _schema = create_test_schema();
        let mut merger = create_test_merger().expect("Failed to create test merger");

        // Prepare an input value definition (argument) type
        let mut input_def = InputValueDefinition {
            name: Name::new("arg").unwrap(),
            description: None,
            default_value: None,
            directives: Default::default(),
            ty: Type::Named(Name::new("Int").unwrap()).into(),
        };
        let mut sources: Sources<Type> = (0..2).map(|i| (i, None)).collect();
        sources.insert(0, Some(Type::Named(Name::new("Int").unwrap())));
        sources.insert(1, Some(Type::NonNullNamed(Name::new("Int").unwrap())));

        // In input position, non-null should be overridden by nullable
        let res = merger.merge_type_reference(
            &sources,
            &mut input_def,
            true,
            Name::new("Parent").unwrap().as_str(),
        );
        assert!(res.is_ok(), "Input position merging should work");
        assert_eq!(
            match input_def.ty.as_ref() {
                Type::Named(n) => n.as_str(),
                _ => "",
            },
            "Int"
        );
    }
}
