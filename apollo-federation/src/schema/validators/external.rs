// the `@external` directive validation

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) fn validate_external_directives(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    validate_no_external_on_interface_fields(schema, metadata, errors)?;
    validate_all_external_fields_used(schema, metadata, errors)?;
    Ok(())
}

fn validate_no_external_on_interface_fields(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for type_name in schema.referencers().interface_types.keys() {
        let type_pos: InterfaceTypeDefinitionPosition =
            schema.get_type(type_name.clone())?.try_into()?;
        for field_pos in type_pos.fields(schema.schema())? {
            let is_external = metadata
                .external_metadata()
                .is_external(&field_pos.clone().into());
            if is_external {
                errors.push(SingleFederationError::ExternalOnInterface {
                    message: format!(
                        r#"Interface type field "{field_pos}" is marked @external but @external is not allowed on interface fields."#
                    ),
                 }.into())
            }
        }
    }
    Ok(())
}

// Checks that all fields marked @external is used in a federation directive (@key, @provides or
// @requires) _or_ to satisfy an interface implementation. Otherwise, the field declaration is
// somewhat useless.
fn validate_all_external_fields_used(
    schema: &FederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for type_pos in schema.get_types() {
        let Ok(type_pos) = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_pos) else {
            continue;
        };
        type_pos.fields(schema.schema())?
            .for_each(|field| {
                let field = field.into();
                if !metadata.is_field_external(&field) || metadata.is_field_used(&field) {
                    return;
                }
                errors.push(SingleFederationError::ExternalUnused {
                    message: format!(
                        r#"Field "{field}" is marked @external but is not used in any federation directive (@key, @provides, @requires) or to satisfy an interface; the field declaration has no use and should be removed (or the field should not be @external)."#
                    ),
                }.into());
            });
    }
    Ok(())
}
