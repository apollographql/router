use std::collections::HashMap;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::OperationType;
use apollo_compiler::ty;
use itertools::Itertools;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Import;
use crate::link::Purpose;
use crate::link::cost_spec_definition::COST_VERSIONS;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::link_spec_definition::LinkSpecDefinition;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::schema::compute_subgraph_metadata;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::cost::validate_cost_directives;
use crate::schema::validators::external::validate_external_directives;
use crate::schema::validators::key::validate_key_directives;
use crate::schema::validators::list_size::validate_list_size_directives;
use crate::schema::validators::provides::validate_provides_directives;
use crate::schema::validators::requires::validate_requires_directives;
use crate::supergraph::GRAPHQL_MUTATION_TYPE_NAME;
use crate::supergraph::GRAPHQL_QUERY_TYPE_NAME;
use crate::supergraph::GRAPHQL_SUBSCRIPTION_TYPE_NAME;
use crate::utils::human_readable::HumanReadableListOptions;
use crate::utils::human_readable::HumanReadableListPrefix;
use crate::utils::human_readable::human_readable_list;

#[allow(dead_code)]
struct CoreFeature {
    url: Url,
    name_in_schema: Name,
    directive: Directive,
    imports: Vec<Import>,
    purpose: Option<Purpose>,
}
#[allow(dead_code)]
pub(crate) struct FederationBlueprint {
    with_root_type_renaming: bool,
}

#[allow(dead_code)]
impl FederationBlueprint {
    pub(crate) fn new(with_root_type_renaming: bool) -> Self {
        Self {
            with_root_type_renaming,
        }
    }

    pub(crate) fn on_missing_directive_definition(
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) -> Result<Option<DirectiveDefinitionPosition>, FederationError> {
        if directive.name == DEFAULT_LINK_NAME {
            let (alias, imports) =
                LinkSpecDefinition::extract_alias_and_imports_on_missing_link_directive_definition(
                    directive,
                )?;
            LinkSpecDefinition::latest().add_definitions_to_schema(schema, alias, imports)?;
            Ok(schema.get_directive_definition(&directive.name))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn on_directive_definition_and_schema_parsed(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
        if federation_spec.is_fed1() {
            Self::remove_federation_definitions_broken_in_known_ways(schema)?;
        }
        federation_spec.add_elements_to_schema(schema)?;
        Self::expand_known_features(schema)
    }

    pub(crate) fn ignore_parsed_field(_type: NamedType, _field_name: &str) -> bool {
        todo!()
    }

    pub(crate) fn on_constructed(schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.subgraph_metadata.is_none() {
            schema.subgraph_metadata = compute_subgraph_metadata(schema)?.map(Box::new);
        }
        Ok(())
    }

    fn on_added_core_feature(_schema: &mut Schema, _feature: &CoreFeature) {
        todo!()
    }

    pub(crate) fn on_invalidation(_: &Schema) {
        todo!()
    }

    pub(crate) fn on_validation(
        &self,
        mut schema: FederationSchema,
    ) -> Result<ValidFederationSchema, FederationError> {
        let mut error_collector = MultipleFederationErrors { errors: Vec::new() };
        if self.with_root_type_renaming {
            let mut operation_types_to_rename = HashMap::new();
            for (op_type, op_name) in schema.schema().schema_definition.iter_root_operations() {
                let default_name = default_operation_name(&op_type);
                if op_name.name != default_name {
                    operation_types_to_rename.insert(op_name.name.clone(), default_name.clone());
                    if schema.try_get_type(default_name.clone()).is_some() {
                        error_collector.push(
                            SingleFederationError::root_already_used(
                                op_type,
                                default_name,
                                op_name.name.clone(),
                            )
                            .into(),
                        );
                    }
                }
            }
            for (current_name, new_name) in operation_types_to_rename {
                schema
                    .get_type(current_name)?
                    .rename(&mut schema, new_name)?;
            }
        }

        let schema = schema.validate_or_return_self().map_err(|e| e.1)?;
        let Some(meta) = schema.subgraph_metadata() else {
            bail!("Federation schema should have had its metadata set on construction");
        };
        // We skip the rest of validation for fed1 schemas because there is a number of validations that is stricter than what fed 1
        // accepted, and some of those issues are fixed by `SchemaUpgrader`. So insofar as any fed 1 schma is ultimately converted
        // to a fed 2 one before composition, then skipping some validation on fed 1 schema is fine.
        if !meta.is_fed_2_schema() {
            return error_collector.into_result().map(|_| schema);
        }

        validate_key_directives(&schema, meta, &mut error_collector)?;
        validate_provides_directives(&schema, meta, &mut error_collector)?;
        validate_requires_directives(&schema, meta, &mut error_collector)?;
        validate_external_directives(&schema, meta, &mut error_collector)?;

        // TODO: Remaining validations
        Self::validate_keys_on_interfaces_are_also_on_all_implementations(
            &schema,
            meta,
            &mut error_collector,
        )?;
        Self::validate_interface_objects_are_on_entities(&schema, meta, &mut error_collector)?;

        validate_cost_directives(&schema, &mut error_collector)?;
        validate_list_size_directives(&schema, &mut error_collector)?;

        error_collector.into_result().map(|_| schema)
    }

    fn on_apollo_rs_validation_error(
        _error: apollo_compiler::validation::WithErrors<Schema>,
    ) -> FederationError {
        todo!()
    }

    fn on_unknown_directive_validation_error(
        _schema: &Schema,
        _unknown_directive_name: &str,
        _error: FederationError,
    ) -> FederationError {
        todo!()
    }

    fn apply_directives_after_parsing() -> bool {
        todo!()
    }

    fn remove_federation_definitions_broken_in_known_ways(
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // We special case @key, @requires and @provides because we've seen existing user schemas where those
        // have been defined in an invalid way, but in a way that fed1 wasn't rejecting. So for convenience,
        // if we detect one of those case, we just remove the definition and let the code afteward add the
        // proper definition back.
        // Note that, in a perfect world, we'd do this within the `SchemaUpgrader`. But the way the code
        // is organised, this method is called before we reach the `SchemaUpgrader`, and it doesn't seem
        // worth refactoring things drastically for that minor convenience.
        for directive_name in &[
            FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC,
            FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC,
        ] {
            if let Some(pos) = schema.get_directive_definition(directive_name) {
                let directive = pos.get(schema.schema())?;
                // We shouldn't have applications at the time of this writing because `completeSubgraphSchema`, which calls this,
                // is only called:
                // 1. during schema parsing, by `FederationBluePrint.onDirectiveDefinitionAndSchemaParsed`, and that is called
                //   before we process any directive applications.
                // 2. by `setSchemaAsFed2Subgraph`, but as the name imply, this trickles to `completeFed2SubgraphSchema`, not
                //   this one method.
                // In other words, there is currently no way to create a full fed1 schema first, and get that method called
                // second. If that changes (no real reason but...), we'd have to modify this because when we remove the
                // definition to re-add the "correct" version, we'd have to re-attach existing applications (doable but not
                // done). This assert is so we notice it quickly if that ever happens (again, unlikely, because fed1 schema
                // is a backward compatibility thing and there is no reason to expand that too much in the future).
                if schema.referencers().get_directive(directive_name)?.len() > 0 {
                    bail!(
                        "Subgraph has applications of @{directive_name} but we are trying to remove the definition."
                    );
                }

                // The patterns we recognize and "correct" (by essentially ignoring the definition) are:
                //  1. if the definition has no arguments at all.
                //  2. if the `fields` argument is declared as nullable.
                //  3. if the `fields` argument type is named "FieldSet" instead of "_FieldSet".
                // All of these correspond to things we've seen in user schemas.
                //
                // To be on the safe side, we check that `fields` is the only argument. That's because
                // fed2 accepts the optional `resolvable` arg for @key, fed1 only ever had one arguemnt.
                // If the user had defined more arguments _and_ provided values for the extra argument,
                // removing the definition would create validation errors that would be hard to understand.
                if directive.arguments.is_empty()
                    || (directive.arguments.len() == 1
                        && directive
                            .argument_by_name(&FEDERATION_FIELDS_ARGUMENT_NAME)
                            .is_some_and(|fields| {
                                *fields.ty == ty!(String)
                                    || *fields.ty == ty!(_FieldSet)
                                    || *fields.ty == ty!(FieldSet)
                            }))
                {
                    pos.remove(schema)?;
                }
            }
        }
        Ok(())
    }

    fn expand_known_features(schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(links_metadata) = schema.metadata() else {
            return Ok(());
        };

        for link in links_metadata.links.clone() {
            // TODO: Pick out known features by link identity and call `add_elements_to_schema`.
            // JS calls coreFeatureDefinitionIfKnown here, but we don't have a feature registry yet.

            if link.url.identity == Identity::cost_identity() {
                let spec = COST_VERSIONS
                    .find(&link.url.version)
                    .ok_or_else(|| SingleFederationError::UnknownLinkVersion {
                        message: format!("Detected unsupported cost specification version {}. Please upgrade to a composition version which supports that version, or select one of the following supported versions: {}.", link.url.version, COST_VERSIONS.versions().join(", "))
                    })?;
                spec.add_elements_to_schema(schema)?;
            }
        }
        Ok(())
    }

    fn validate_keys_on_interfaces_are_also_on_all_implementations(
        schema: &ValidFederationSchema,
        metadata: &SubgraphMetadata,
        error_collector: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        let key_directive_definition_name = &metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?
            .name;
        for type_pos in schema.get_types() {
            let Ok(type_pos): Result<InterfaceTypeDefinitionPosition, _> = type_pos.try_into()
            else {
                continue;
            };
            let implementation_types = schema.possible_runtime_types(type_pos.clone().into())?;
            let type_ = type_pos.get(schema.schema())?;
            for application in type_.directives.get_all(key_directive_definition_name) {
                let arguments = metadata
                    .federation_spec_definition()
                    .key_directive_arguments(application)?;
                // Note that we will have validated all @key field sets by this point, so we skip
                // re-validating here.
                let fields = parse_field_set(schema, type_.name.clone(), arguments.fields, false)?;
                let mut implementations_with_non_resolvable_keys = vec![];
                let mut implementations_with_missing_keys = vec![];
                for implementation_type_pos in &implementation_types {
                    let implementation_type = implementation_type_pos.get(schema.schema())?;
                    let mut matching_application_arguments = None;
                    for implementation_application in implementation_type
                        .directives
                        .get_all(key_directive_definition_name)
                    {
                        let implementation_arguments = metadata
                            .federation_spec_definition()
                            .key_directive_arguments(implementation_application)?;
                        let implementation_fields = parse_field_set(
                            schema,
                            implementation_type.name.clone(),
                            implementation_arguments.fields,
                            false,
                        )?;
                        if implementation_fields == fields {
                            matching_application_arguments = Some(implementation_arguments);
                            break;
                        }
                    }
                    if let Some(matching_application_arguments) = matching_application_arguments {
                        // TODO: This code assumes there's at most one matching application for a
                        // given fieldset, but I'm not sure whether other validation code guarantees
                        // this.
                        if arguments.resolvable && !matching_application_arguments.resolvable {
                            implementations_with_non_resolvable_keys.push(implementation_type_pos);
                        }
                    } else {
                        implementations_with_missing_keys.push(implementation_type_pos);
                    }

                    if !implementations_with_missing_keys.is_empty() {
                        let types_list = human_readable_list(
                            implementations_with_missing_keys
                                .iter()
                                .map(|pos| format!("\"{}\"", pos)),
                            HumanReadableListOptions {
                                prefix: Some(HumanReadableListPrefix {
                                    singular: "type",
                                    plural: "types",
                                }),
                                ..Default::default()
                            },
                        );
                        error_collector.errors.push(
                            SingleFederationError::InterfaceKeyNotOnImplementation {
                                message: format!(
                                    "Key {} on interface type \"{}\" is missing on implementation {}",
                                    application.serialize(),
                                    type_pos,
                                    types_list,
                                )
                            }
                        )
                    } else if !implementations_with_non_resolvable_keys.is_empty() {
                        let types_list = human_readable_list(
                            implementations_with_non_resolvable_keys
                                .iter()
                                .map(|pos| format!("\"{}\"", pos)),
                            HumanReadableListOptions {
                                prefix: Some(HumanReadableListPrefix {
                                    singular: "type",
                                    plural: "types",
                                }),
                                ..Default::default()
                            },
                        );
                        error_collector.errors.push(
                            SingleFederationError::InterfaceKeyNotOnImplementation {
                                message: format!(
                                    "Key {} on interface type \"{}\" should be resolvable on all implementation types, but is declared with argument \"@key(resolvable:)\" set to false in {}",
                                    application.serialize(),
                                    type_pos,
                                    types_list,
                                )
                            }
                        )
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_interface_objects_are_on_entities(
        schema: &ValidFederationSchema,
        metadata: &SubgraphMetadata,
        error_collector: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        let Some(interface_object_directive_definition) = &metadata
            .federation_spec_definition()
            .interface_object_directive_definition(schema)?
        else {
            return Ok(());
        };
        let key_directive_definition_name = &metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?
            .name;
        for type_pos in &schema
            .referencers
            .get_directive(&interface_object_directive_definition.name)?
            .object_types
        {
            if !type_pos
                .get(schema.schema())?
                .directives
                .has(key_directive_definition_name)
            {
                error_collector.errors.push(
                    SingleFederationError::InterfaceObjectUsageError {
                        message: format!(
                            "The @interfaceObject directive can only be applied to entity types but type \"{}\" has no @key in this subgraph.",
                            type_pos
                        )
                    }
                )
            }
        }
        Ok(())
    }
}

fn default_operation_name(op_type: &OperationType) -> Name {
    match op_type {
        OperationType::Query => GRAPHQL_QUERY_TYPE_NAME,
        OperationType::Mutation => GRAPHQL_MUTATION_TYPE_NAME,
        OperationType::Subscription => GRAPHQL_SUBSCRIPTION_TYPE_NAME,
    }
}
