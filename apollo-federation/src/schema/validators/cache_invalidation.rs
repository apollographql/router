use std::fmt;
use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::ast;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::LineColumn;

use crate::bail;
use crate::connectors::ConnectSpec;
use crate::connectors::SelectionTrie;
use crate::connectors::StringTemplate;
use crate::connectors::spec::connect_spec_from_schema;
use crate::error::ErrorCode;
use crate::error::FederationError;
use crate::link::federation_spec_definition::CacheInvalidationDirectiveArguments;
use crate::schema::FederationSchema;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;

const DEFAULT_CONNECT_SPEC: ConnectSpec = ConnectSpec::V0_2;

pub(crate) fn validate_cache_invalidation_directives(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
) -> Result<(), FederationError> {
    let applications = schema.cache_invalidation_directive_applications()?;
    for application in applications {
        match application {
            Ok(cache_tag_directive) => match &cache_tag_directive.target {
                DirectiveTargetPosition::ObjectField(field) => validate_application_on_field(
                    schema,
                    errors,
                    field,
                    &cache_tag_directive.arguments,
                )?,
                _ => bail!("Unexpected directive target"),
            },
            Err(error) => errors.push(Message {
                error: CacheInvalidationValidationError::FederationError { error },
                locations: Vec::new(),
            }),
        }
    }
    Ok(())
}

fn validate_application_on_field(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    field: &ObjectFieldDefinitionPosition,
    args: &CacheInvalidationDirectiveArguments,
) -> Result<(), FederationError> {
    let field_def = field.get(schema.schema())?;

    // validate it's on a root field
    if !schema.is_mutation_root_type(&field.type_name) {
        let error = CacheInvalidationValidationError::NotOnRootField {
            type_name: field.type_name.clone(),
            field_name: field.field_name.clone(),
        };
        errors.push(Message::new(schema, field_def, error));
        return Ok(());
    }

    if args.cache_tag.is_none() && args.r#type.is_none() {
        let error = CacheInvalidationValidationError::InvalidArguments {
            type_name: field.type_name.clone(),
            field_name: field.field_name.clone(),
        };
        errors.push(Message::new(schema, field_def, error));
        return Ok(());
    }

    // validate the arguments
    validate_args_on_field(schema, errors, field, args)?;
    Ok(())
}

fn validate_args_on_field(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    field: &ObjectFieldDefinitionPosition,
    args: &CacheInvalidationDirectiveArguments,
) -> Result<(), FederationError> {
    let field_def = field.get(schema.schema())?;
    let connect_spec = connect_spec_from_schema(schema.schema()).unwrap_or(DEFAULT_CONNECT_SPEC);
    if let Some(cache_tag) = args.cache_tag {
        validate_arg_on_field(
            schema,
            errors,
            field,
            "cacheTag",
            cache_tag,
            field_def,
            connect_spec,
        )?;
    }
    if let Some(r#type) = args.r#type {
        validate_arg_on_field(
            schema,
            errors,
            field,
            "type",
            r#type,
            field_def,
            connect_spec,
        )?;
    }

    Ok(())
}

fn validate_arg_on_field(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    field: &ObjectFieldDefinitionPosition,
    arg_name: &'static str,
    arg_input: &str,
    field_def: &apollo_compiler::schema::Component<ast::FieldDefinition>,
    connect_spec: ConnectSpec,
) -> Result<(), FederationError> {
    let arg_value = match StringTemplate::parse_with_spec(arg_input, connect_spec) {
        Ok(format) => format,
        Err(err) => {
            errors.push(Message::new(
                schema,
                field_def,
                CacheInvalidationValidationError::InvalidArgument {
                    arg_name,
                    message: err.message,
                },
            ));
            return Ok(());
        }
    };
    let new_errors = arg_value.expressions().filter_map(|expr| {
        expr.expression.if_named_else_path(
            |_named| {
                Some(CacheInvalidationValidationError::InvalidArgument {
                    arg_name,
                    message: format!("\"{}\"", expr.expression),
                })
            },
            |path| match path.variable_reference::<String>() {
                Some(var_ref) => {
                    // Check the namespace
                    if var_ref.namespace.namespace != "$args" {
                        return Some(
                            CacheInvalidationValidationError::InvalidArgumentOnRootField {
                                arg_name,
                            },
                        );
                    }

                    // Check the selection
                    let fields = field_def
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.clone(), arg.ty.as_ref()))
                        .collect::<IndexMap<Name, &Type>>();
                    match validate_args_selection(arg_name, schema, &fields, &var_ref.selection) {
                        Ok(_) => None,
                        Err(_err) => Some(CacheInvalidationValidationError::ArgumentUnknown {
                            type_name: field.type_name.clone(),
                            field_name: field.field_name.clone(),
                            arg_name,
                            arg_value: arg_value.to_string(),
                        }),
                    }
                }
                None => None,
            },
        )
    });
    errors.extend(new_errors.map(|err| Message::new(schema, field_def, err)));

    Ok(())
}

fn validate_args_selection(
    // Argument name in the cacheInvalidation directive (useful for error messsage)
    arg_name: &'static str,
    schema: &FederationSchema,
    fields: &IndexMap<Name, &Type>,
    selection: &SelectionTrie,
) -> Result<(), CacheInvalidationValidationError> {
    for (key, sel) in selection.iter() {
        let name =
            Name::new(key).map_err(|_| CacheInvalidationValidationError::InvalidArgument {
                arg_name,
                message: format!("invalid field selection name \"{key}\""),
            })?;
        let field =
            fields
                .get(&name)
                .ok_or_else(|| CacheInvalidationValidationError::InvalidArgument {
                    arg_name,
                    message: format!("unknown field \"{name}\""),
                })?;
        let is_nullable = matches!(field, Type::Named(_) | Type::List(_));
        if is_nullable {
            return Err(CacheInvalidationValidationError::NullableArguments {
                arg_name,
                field_name: name.clone(),
            });
        }
        let type_name = field.inner_named_type();
        let type_def = schema.get_type(type_name.clone())?;
        if !sel.is_leaf() {
            let type_def = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_def).map_err(
                |_| CacheInvalidationValidationError::InvalidArgument {
                    arg_name,
                    message: format!(
                        "invalid path element \"{name}\", which is not an object or interface type"
                    ),
                },
            )?;
            let next_fields = type_def
                .fields(schema.schema())?
                .map(|field_pos| {
                    let field_def = field_pos
                        .get(schema.schema())
                        .map_err(FederationError::from)?;
                    Ok::<_, CacheInvalidationValidationError>((
                        field_pos.field_name().clone(),
                        &field_def.ty,
                    ))
                })
                .collect::<Result<IndexMap<_, _>, _>>()?;
            validate_args_selection(arg_name, schema, &next_fields, sel)?;
        } else {
            // A leaf field should have a scalar type.
            if !matches!(&type_def, TypeDefinitionPosition::Scalar(_)) {
                return Err(CacheInvalidationValidationError::InvalidArgument {
                    arg_name,
                    message: format!(
                        "invalid path ending at \"{name}\", which is not a scalar type"
                    ),
                });
            }
        }
    }
    Ok(())
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Error messages

// Note: This is modeled after the Connectors' `Message` struct.
#[derive(Debug, Clone)]
pub struct Message {
    error: CacheInvalidationValidationError,
    pub locations: Vec<Range<LineColumn>>,
}

impl Message {
    fn new<T>(
        schema: &FederationSchema,
        node: &apollo_compiler::Node<T>,
        error: CacheInvalidationValidationError,
    ) -> Self {
        Self {
            error,
            locations: schema.node_locations(node).collect(),
        }
    }

    pub fn code(&self) -> String {
        self.error.code()
    }

    pub fn message(&self) -> String {
        self.error.to_string()
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

/// `@cacheTag` validation errors
// Note: This is expected to be merged with `CompositionError` and Connectors errors later.
#[derive(Debug, Clone, thiserror::Error, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
enum CacheInvalidationValidationError {
    #[error("{error}")]
    FederationError { error: FederationError },
    #[error("{arg_name} format is invalid: {message}")]
    InvalidArgument {
        arg_name: &'static str,
        message: String,
    },
    #[error("cacheInvalidation {arg_name} because it uses nullable argument \"{field_name}\"")]
    NullableArguments {
        arg_name: &'static str,
        field_name: Name,
    },
    #[error(
        "error on field \"{field_name}\" on type \"{type_name}\": cacheInvalidation should either have \"cacheTag\" argument or \"type\" argument set"
    )]
    InvalidArguments { type_name: Name, field_name: Name },
    #[error(
        "error on field \"{field_name}\" on type \"{type_name}\": cacheTag can only apply on root fields"
    )]
    NotOnRootField { type_name: Name, field_name: Name },
    #[error(
        "cacheInvalidation applied on mutation root fields can only reference arguments in {arg_name} using $args"
    )]
    InvalidArgumentOnRootField { arg_name: &'static str },
    #[error(
        "Unknown arguments used with $args in cacheInvalidation {arg_name} \"{arg_value}\" on field \"{field_name}\" for type \"{type_name}\""
    )]
    ArgumentUnknown {
        type_name: Name,
        field_name: Name,
        arg_name: &'static str,
        arg_value: String,
    },
}

impl CacheInvalidationValidationError {
    fn code(&self) -> String {
        match self {
            // Special handling for FederationError
            CacheInvalidationValidationError::FederationError { error } => match error {
                FederationError::SingleFederationError(inner) => {
                    inner.code().definition().code().to_string()
                }
                FederationError::MultipleFederationErrors(inner) => {
                    let code = match inner.errors.first() {
                        // Error is unexpectedly empty. Treat it as an internal error.
                        None => ErrorCode::Internal,
                        Some(e) => e.code(),
                    };
                    // Convert to string
                    code.definition().code().to_string()
                }
                FederationError::AggregateFederationError(inner) => inner.code.clone(),
            },
            // For the rest of cases
            _ => {
                let code: &str = self.into();
                code.to_string()
            }
        }
    }
}

impl From<FederationError> for CacheInvalidationValidationError {
    fn from(error: FederationError) -> Self {
        CacheInvalidationValidationError::FederationError { error }
    }
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Unit tests

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::subgraph::test_utils::BuildOption;
//     use crate::subgraph::test_utils::build_inner_expanded;

//     #[test]
//     fn test_api_test() {
//         const SCHEMA: &str = r#"
//             type Product @key(fields: "upc")
//                          @cacheTag(format: "{ namedField }")
//                          @cacheTag(format: "{$args}")
//             {
//                 upc: String!
//                 name: String
//             }

//             type Query {
//                 topProducts(first: Int = 5): [Product]
//                     @cacheTag(format: "{$this}")
//                     @cacheTag(format: "{$key}")
//             }
//         "#;

//         let subgraph = build_inner_expanded(SCHEMA, BuildOption::AsFed2).unwrap();
//         let mut errors = Vec::new();
//         validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
//         assert_eq!(
//             errors.iter().map(|e| e.code()).collect::<Vec<_>>(),
//             vec![
//                 "CACHE_TAG_INVALID_FORMAT",
//                 "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ENTITY",
//                 "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ROOT_FIELD",
//                 "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ROOT_FIELD",
//             ],
//         );
//     }

//     #[track_caller]
//     fn build_and_validate(schema: &str) {
//         let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
//         let mut errors = Vec::new();
//         validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
//         assert!(errors.is_empty());
//     }

//     #[track_caller]
//     fn build_for_errors(schema: &str) -> Vec<String> {
//         let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
//         let mut errors = Vec::new();
//         validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
//         errors.iter().map(|e| e.to_string()).collect()
//     }

//     #[test]
//     fn test_valid_format_string() {
//         const SCHEMA: &str = r#"
//             type Product @key(fields: "upc")
//                          @cacheTag(format: "product-{$key.upc}")
//             {
//                 upc: String!
//                 name: String
//             }

//             type Query {
//                 topProducts(first: Int = 5): [Product]
//                     @cacheTag(format: "topProducts")
//                     @cacheTag(format: "topProducts-{$args.first}")
//             }
//         "#;
//         build_and_validate(SCHEMA);
//     }

//     #[test]
//     fn test_invalid_format_selection() {
//         const SCHEMA: &str = r#"
//             type Product @key(fields: "upc")
//                          @cacheTag(format: "{ namedField }")
//                          @cacheTag(format: "{$args}")
//             {
//                 upc: String!
//                 name: String
//             }

//             type Query {
//                 topProducts(first: Int = 5): [Product]
//                     @cacheTag(format: "{$this}")
//                     @cacheTag(format: "{$key}")
//             }
//         "#;
//         assert_eq!(
//             build_for_errors(SCHEMA),
//             vec![
//                 "cacheTag format is invalid: \"namedField\"",
//                 "cacheTag applied on types can only reference arguments in format using $key",
//                 "cacheTag applied on root fields can only reference arguments in format using $args",
//                 "cacheTag applied on root fields can only reference arguments in format using $args",
//             ]
//         );
//     }

//     #[test]
//     fn test_invalid_format_path_selection() {
//         const SCHEMA: &str = r#"
//             type Test {
//                 a: Int!
//                 b: Int!
//             }

//             type Product @key(fields: "upc test { a }")
//                          @cacheTag(format: "product-{$key.somethingElse}")
//                          @cacheTag(format: "product-{$key.test}")
//                          @cacheTag(format: "product-{$key.test.a}")
//                          @cacheTag(format: "product-{$key.test.b}")
//             {
//                 upc: String!
//                 test: Test!
//                 name: String
//             }

//             type Query {
//                 topProducts(first: Int = 5): [Product]
//                     @cacheTag(format: "topProducts")
//                     @cacheTag(format: "topProducts-{$args.second}")
//             }
//         "#;
//         assert_eq!(
//             build_for_errors(SCHEMA),
//             vec![
//                 "cacheTag format is invalid: cannot create selection set with \"somethingElse\"",
//                 "cacheTag format is invalid: invalid path ending at \"test\", which is not a scalar type",
//                 "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"product-{$key.test.b}\" on type \"Product\"",
//                 "Unknown arguments used with $args in cacheTag format \"topProducts-{$args.second}\" on field \"topProducts\" for type \"Query\"",
//             ]
//         );
//     }

//     #[test]
//     fn test_valid_format_string_multiple_keys() {
//         const SCHEMA: &str = r#"
//             type Product @key(fields: "upc x")
//                          @key(fields: "upc y")
//                          @cacheTag(format: "product-{$key.upc}")
//             {
//                 upc: String!
//                 x: Int!
//                 y: Int!
//                 name: String
//             }
//         "#;
//         build_and_validate(SCHEMA);
//     }

//     #[test]
//     fn test_invalid_format_string_multiple_keys() {
//         const SCHEMA: &str = r#"
//             type Product @key(fields: "upc x")
//                          @key(fields: "upc y")
//                          @cacheTag(format: "product-{$key.x}")
//             {
//                 upc: String!
//                 x: Int!
//                 y: Int!
//                 name: String
//             }
//         "#;
//         assert_eq!(
//             build_for_errors(SCHEMA),
//             vec![
//                 "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"product-{$key.x}\" on type \"Product\""
//             ]
//         );
//     }

//     #[test]
//     fn test_latest_connect_spec() {
//         // This test exists to find out when ConnectSpec::latest() changes, so
//         // we can decide whether to update DEFAULT_CONNECT_SPEC.
//         assert_eq!(DEFAULT_CONNECT_SPEC, ConnectSpec::latest());
//     }
// }
