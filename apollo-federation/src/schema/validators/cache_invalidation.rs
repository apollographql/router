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
            "cacheTag",
            cache_tag,
            field_def,
            connect_spec,
        )?;
    }
    if let Some(r#type) = args.r#type {
        validate_arg_on_field(schema, errors, "type", r#type, field_def, connect_spec)?;
    }

    Ok(())
}

fn validate_arg_on_field(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
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
                    validate_args_selection(arg_name, schema, &fields, &var_ref.selection).err()
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
    #[error("@cacheInvalidation {arg_name} format is invalid: {message}")]
    InvalidArgument {
        arg_name: &'static str,
        message: String,
    },
    #[error(
        "@cacheInvalidation can only use non nullable argument but \"{field_name}\" in {arg_name} is nullable"
    )]
    NullableArguments {
        arg_name: &'static str,
        field_name: Name,
    },
    #[error(
        "error on field \"{field_name}\" on type \"{type_name}\": @cacheInvalidation should either have \"cacheTag\" argument or \"type\" argument set"
    )]
    InvalidArguments { type_name: Name, field_name: Name },
    #[error(
        "error on field \"{field_name}\" on type \"{type_name}\": @cacheInvalidation can only apply on root fields"
    )]
    NotOnRootField { type_name: Name, field_name: Name },
    #[error(
        "@cacheInvalidation applied on mutation root fields can only reference arguments in {arg_name} using $args"
    )]
    InvalidArgumentOnRootField { arg_name: &'static str },
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
                format!("CACHE_INVALIDATION_{code}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgraph::test_utils::BuildOption;
    use crate::subgraph::test_utils::build_inner_expanded;

    #[test]
    fn test_api_test() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc")
            {
                upc: String!
                name: String
            }

            type Mutation {
                updateProduct(productUpc: String!): [Product]
                    @cacheInvalidation(cacheTag: "{$this}")
                    @cacheInvalidation(cacheTag: "{$key}")
            }
        "#;

        let subgraph = build_inner_expanded(SCHEMA, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_invalidation_directives(subgraph.schema(), &mut errors).unwrap();
        assert_eq!(
            errors.iter().map(|e| e.code()).collect::<Vec<_>>(),
            vec![
                "CACHE_INVALIDATION_INVALID_ARGUMENT_ON_ROOT_FIELD",
                "CACHE_INVALIDATION_INVALID_ARGUMENT_ON_ROOT_FIELD"
            ],
        );
    }

    #[track_caller]
    fn build_and_validate(schema: &str) {
        let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_invalidation_directives(subgraph.schema(), &mut errors).unwrap();
        assert!(errors.is_empty());
    }

    #[track_caller]
    fn build_for_errors(schema: &str) -> Vec<String> {
        let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_invalidation_directives(subgraph.schema(), &mut errors).unwrap();
        errors.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn test_valid_format_string() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc")
            {
                upc: String!
                name: String
            }

            type Mutation {
                updateProduct(productUpc: String!): [Product]
                    @cacheInvalidation(cacheTag: "product")
                    @cacheInvalidation(type: "Product")
                    @cacheInvalidation(cacheTag: "product-{$args.productUpc}")
            }
        "#;
        build_and_validate(SCHEMA);
    }

    #[test]
    fn test_invalid_format_selection() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc")
            {
                upc: String!
                name: String
            }

            type Mutation {
                updateProduct(productUpc: String): [Product]
                    @cacheInvalidation(cacheTag: "{$this}")
                    @cacheInvalidation(cacheTag: "{$key}")
                    @cacheInvalidation(cacheTag: "product-{$args.productUpc}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "@cacheInvalidation applied on mutation root fields can only reference arguments in cacheTag using $args",
                "@cacheInvalidation applied on mutation root fields can only reference arguments in cacheTag using $args",
                "@cacheInvalidation can only use non nullable argument but \"productUpc\" in cacheTag is nullable"
            ]
        );
    }

    #[test]
    fn test_invalid_format_path_selection() {
        const SCHEMA: &str = r#"
            type Test {
                a: Int!
                b: Int!
            }

            type Product @key(fields: "upc test { a }")
            {
                upc: String!
                test: Test!
                name: String
            }

            type Mutation {
                updateProduct(product: Product!): Product
                    @cacheInvalidation(cacheTag: "{$args.product.somethingElse}")
                    @cacheInvalidation(cacheTag: "{$args.product.test}")
                    @cacheInvalidation(cacheTag: "{$args.product.test.a}")
                    @cacheInvalidation(cacheTag: "{$args.product.test.b}")
                    @cacheInvalidation(cacheTag: "{$args.product.name}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "@cacheInvalidation cacheTag format is invalid: unknown field \"somethingElse\"",
                "@cacheInvalidation cacheTag format is invalid: invalid path ending at \"test\", which is not a scalar type",
                "@cacheInvalidation can only use non nullable argument but \"name\" in cacheTag is nullable"
            ]
        );
    }

    #[test]
    fn test_latest_connect_spec() {
        // This test exists to find out when ConnectSpec::latest() changes, so
        // we can decide whether to update DEFAULT_CONNECT_SPEC.
        assert_eq!(DEFAULT_CONNECT_SPEC, ConnectSpec::latest());
    }
}
