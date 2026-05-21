use apollo_compiler::name;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::cost_spec_definition::CostSpecDefinition;
use crate::link::cost_spec_definition::CostWeightValue;
use crate::schema::FederationSchema;
use crate::schema::position::HasAppliedDirectives;

pub(crate) fn validate_cost_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let Some(cost_directive_name) = CostSpecDefinition::cost_directive_name(schema) else {
        return Ok(());
    };
    let cost_directive_referencers = schema
        .referencers()
        .get_directive(cost_directive_name.as_str());
    for interface_field in &cost_directive_referencers.interface_fields {
        errors
            .errors
            .push(SingleFederationError::CostAppliedToInterfaceField {
                interface: interface_field.type_name.clone(),
                field: interface_field.field_name.clone(),
            });
    }
    for target in cost_directive_referencers.iter() {
        for directive in target.get_applied_directives(schema, &cost_directive_name) {
            if let Some(weight) = directive
                .as_ref()
                .specified_argument_by_name(&name!("weight"))
                && weight.as_ref().cost_weight().is_none()
            {
                errors.errors.push(SingleFederationError::InvalidGraphQL {
                    message: format!(
                        r#"@cost weight on "{target}" must be a serialized finite Float"#
                    ),
                });
            }
        }
    }
    Ok(())
}
