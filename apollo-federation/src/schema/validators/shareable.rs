use apollo_compiler::Name;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC;
use crate::schema::FederationSchema;
use crate::schema::referencer::DirectiveReferencers;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) fn validate_shareable_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let directive_name = meta
        .federation_spec_definition()
        .shareable_directive_name_in_schema(schema)?
        .unwrap_or(FEDERATION_SHAREABLE_DIRECTIVE_NAME_IN_SPEC);
    let shareable_referencers = schema.referencers().get_directive(&directive_name)?;

    validate_shareable_not_repeated_on_same_type_declaration(
        schema,
        &directive_name,
        shareable_referencers,
        errors,
    )?;
    validate_shareable_not_repeated_on_same_field_declaration(
        schema,
        &directive_name,
        shareable_referencers,
        errors,
    )?;
    validate_shareable_not_applied_to_interface_fields(
        schema,
        &directive_name,
        shareable_referencers,
        errors,
    )?;

    Ok(())
}

fn validate_shareable_not_repeated_on_same_type_declaration(
    schema: &FederationSchema,
    directive_name: &Name,
    shareable_referencers: &DirectiveReferencers,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for pos in shareable_referencers.object_types.iter() {
        let shareable_applications = pos.get_applied_directives(schema, directive_name);
        let count_by_extension = shareable_applications
            .iter()
            .counts_by(|x| x.origin.extension_id());
        if count_by_extension.iter().any(|(_, count)| *count > 1) {
            errors.push(
                SingleFederationError::InvalidShareableUsage {
                    message: format!("Invalid duplicate application of @shareable on the same type declaration of \"{}\": @shareable is only repeatable on types so it can be used simultaneously on a type definition and its extensions, but it should not be duplicated on the same definition/extension declaration", pos.type_name)
                }.into(),
            );
        }
    }

    Ok(())
}

fn validate_shareable_not_repeated_on_same_field_declaration(
    schema: &FederationSchema,
    directive_name: &Name,
    shareable_referencers: &DirectiveReferencers,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for pos in shareable_referencers.object_fields.iter() {
        let shareable_applications = pos.get_applied_directives(schema, directive_name);
        if shareable_applications.len() > 1 {
            errors.push(
                SingleFederationError::InvalidShareableUsage {
                    message: format!("Invalid duplicate application of @shareable on field \"{}.{}\": @shareable is only repeatable on types so it can be used simultaneously on a type definition and its extensions, but it should not be duplicated on the same definition/extension declaration", pos.type_name, pos.field_name)
                }.into(),
            );
        }
    }

    Ok(())
}

fn validate_shareable_not_applied_to_interface_fields(
    schema: &FederationSchema,
    directive_name: &Name,
    shareable_referencers: &DirectiveReferencers,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for pos in shareable_referencers.interface_fields.iter() {
        let shareable_applications = pos.get_applied_directives(schema, directive_name);
        if !shareable_applications.is_empty() {
            errors.push(
                SingleFederationError::InvalidShareableUsage {
                    message: format!("Invalid use of @shareable on field \"{}.{}\": only object type fields can be marked with @shareable", pos.type_name, pos.field_name)
                }.into(),
            );
        }
    }

    Ok(())
}
