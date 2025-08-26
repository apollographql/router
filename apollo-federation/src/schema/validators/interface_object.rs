use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::utils::human_readable::HumanReadableListOptions;
use crate::utils::human_readable::HumanReadableListPrefix;
use crate::utils::human_readable::human_readable_list;

pub(crate) fn validate_interface_object_directives(
    schema: &ValidFederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    validate_keys_on_interfaces_are_also_on_all_implementations(schema, metadata, errors)?;
    validate_interface_objects_are_on_entities(schema, metadata, errors)?;
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
        let Ok(type_pos) = InterfaceTypeDefinitionPosition::try_from(type_pos) else {
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
            }

            if !implementations_with_missing_keys.is_empty() {
                let types_list = human_readable_list(
                    implementations_with_missing_keys
                        .iter()
                        .map(|pos| format!("\"{pos}\"")),
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
                            "Key {} on interface type \"{}\" is missing on implementation {}.",
                            application.serialize(),
                            type_pos,
                            types_list,
                        ),
                    },
                )
            } else if !implementations_with_non_resolvable_keys.is_empty() {
                let types_list = human_readable_list(
                    implementations_with_non_resolvable_keys
                        .iter()
                        .map(|pos| format!("\"{pos}\"")),
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
                                "Key {} on interface type \"{}\" should be resolvable on all implementation types, but is declared with argument \"@key(resolvable:)\" set to false in {}.",
                                application.serialize(),
                                type_pos,
                                types_list,
                            )
                        }
                    )
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
                        "The @interfaceObject directive can only be applied to entity types but type \"{type_pos}\" has no @key in this subgraph."
                    )
                }
            )
        }
    }
    Ok(())
}
