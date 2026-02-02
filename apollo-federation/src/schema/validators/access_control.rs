use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::authenticated_spec_definition::AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::policy_spec_definition::POLICY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::ValidFederationSchema;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) fn validate_no_access_control_on_interfaces(
    schema: &ValidFederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
    for directive in [
        AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
        REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
        POLICY_DIRECTIVE_NAME_IN_SPEC,
    ] {
        if let Some(directive_name) =
            federation_spec.directive_name_in_schema(schema, &directive)?
        {
            let references = schema.referencers().get_directive(&directive_name);
            for interface_field in &references.interface_fields {
                errors
                    .errors
                    .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                        directive_name: directive_name.to_string(),
                        coordinate: interface_field.to_string(),
                        kind: "field".to_string(),
                    })
            }
            for interface_type in &references.interface_types {
                errors
                    .errors
                    .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                        directive_name: directive_name.to_string(),
                        coordinate: interface_type.to_string(),
                        kind: "interface".to_string(),
                    })
            }
            for object_type in &references.object_types {
                if metadata.is_interface_object_type(&object_type.type_name) {
                    errors
                        .errors
                        .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                            directive_name: directive_name.to_string(),
                            coordinate: object_type.to_string(),
                            kind: "interface object".to_string(),
                        })
                }
            }
        }
    }
    Ok(())
}
