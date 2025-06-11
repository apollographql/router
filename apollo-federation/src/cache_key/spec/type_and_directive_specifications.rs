use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::ty;

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
    // cacheKey/v0.1:
    // directive @cacheKey(
    //   format: String
    //   cascade: Boolean = false
    // ) repeatable on FIELD_DEFINITION | OBJECT
    let cache_key_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&name!("cacheKey")),
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("format"),
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("cascade"),
                    get_type: |_, _| Ok(Type::Named(name!(Boolean))),
                    default_value: Some(Value::Boolean(false)),
                },
                composition_strategy: None,
            },
        ],
        true,
        &[
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
        ],
        false,
        None,
        None,
    );

    cache_key_spec.check_or_add(schema, None)?;

    Ok(())
}
