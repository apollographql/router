use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::name;
use apollo_compiler::ty;

use crate::cache_tag::spec::schema::CACHE_TAG_DIRECTIVE_NAME_IN_SPEC;
use crate::error::FederationError;
use crate::link::Link;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(super) fn check_or_add(
    link: &Link,
    schema: &mut FederationSchema,
) -> Result<(), FederationError> {
    // cacheTag/v0.1:
    // directive @cacheTag(
    //   format: String
    // ) repeatable on FIELD_DEFINITION | OBJECT
    let cache_tag_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&CACHE_TAG_DIRECTIVE_NAME_IN_SPEC),
        &[DirectiveArgumentSpecification {
            base_spec: ArgumentSpecification {
                name: name!("format"),
                get_type: |_, _| Ok(ty!(String)),
                default_value: None,
            },
            composition_strategy: None,
        }],
        true,
        &[
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
        ],
        false,
        None,
        None,
    );

    cache_tag_spec.check_or_add(schema, None)?;

    Ok(())
}
