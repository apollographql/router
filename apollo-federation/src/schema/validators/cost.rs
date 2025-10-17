use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::cost_spec_definition::CostSpecDefinition;
use crate::schema::FederationSchema;

pub(crate) fn validate_cost_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let Some(cost_directive_name) = CostSpecDefinition::cost_directive_name(schema)? else {
        return Ok(());
    };
    let Ok(cost_directive_referencers) = schema
        .referencers()
        .get_directive(cost_directive_name.as_str())
    else {
        // This just returns an Err if the directive is not found, which is fine in this case.
        return Ok(());
    };
    for interface_field in &cost_directive_referencers.interface_fields {
        errors
            .errors
            .push(SingleFederationError::CostAppliedToInterfaceField {
                interface: interface_field.type_name.clone(),
                field: interface_field.field_name.clone(),
            });
    }
    Ok(())
}
