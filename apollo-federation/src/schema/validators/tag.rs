use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::link::tag_spec_definition::TAG_VERSIONS;
use crate::schema::FederationSchema;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::type_and_directive_specification::ensure_same_directive_structure;

/// If `@tag` is redefined by the user, make sure the definition is compatible with the specification.
pub(crate) fn validate_tag_directive_is_spec_compliant(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let Ok(tag_definition) = meta
        .federation_spec_definition()
        .tag_directive_definition(schema)
    else {
        return Ok(()); // No tag directive in schema
    };

    // TODO: Should we always use latest?
    let tag_definition_from_spec = TAG_VERSIONS.latest().tag_directive_specification();
    let mut tag_definition_from_spec_args = Vec::with_capacity(tag_definition_from_spec.args.len());
    for arg in tag_definition_from_spec.args.iter() {
        match arg
            .base_spec
            .resolve(schema, None) // TODO: does this need a link?
        {
            Ok(resolved_arg) => tag_definition_from_spec_args.push(resolved_arg),
            Err(err) => {
                errors.push(err);
            }
        }
    }

    if let Err(e) = ensure_same_directive_structure(
        tag_definition,
        &tag_definition_from_spec.name,
        tag_definition_from_spec_args.as_slice(),
        tag_definition_from_spec.repeatable,
        &tag_definition_from_spec.locations,
        schema,
    ) {
        errors.push(e);
    }
    Ok(())
}
