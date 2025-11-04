use std::fmt;
use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::executable;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use crate::bail;
use crate::connectors::ConnectSpec;
use crate::connectors::SelectionTrie;
use crate::connectors::StringTemplate;
use crate::connectors::StringTemplateError;
use crate::connectors::spec::connect_spec_from_schema;
use crate::error::ErrorCode;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::CacheTagDirectiveArguments;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::FederationSchema;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;

const DEFAULT_CONNECT_SPEC: ConnectSpec = ConnectSpec::V0_2;

pub(crate) fn validate_cache_tag_directives(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
) -> Result<(), FederationError> {
    let applications = schema.cache_tag_directive_applications()?;
    for application in applications {
        match application {
            Ok(cache_tag_directive) => match &cache_tag_directive.target {
                DirectiveTargetPosition::ObjectField(field) => validate_application_on_field(
                    schema,
                    errors,
                    field,
                    &cache_tag_directive.arguments,
                )?,
                DirectiveTargetPosition::ObjectType(type_pos) => {
                    validate_application_on_object_type(
                        schema,
                        errors,
                        type_pos,
                        &cache_tag_directive.arguments,
                    )?;
                }
                _ => bail!("Unexpected directive target"),
            },
            Err(error) => errors.push(Message {
                error: CacheTagValidationError::FederationError { error },
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
    args: &CacheTagDirectiveArguments,
) -> Result<(), FederationError> {
    let field_def = field.get(schema.schema())?;

    // validate it's on a root field
    if !schema.is_root_type(&field.type_name) {
        let error = CacheTagValidationError::CacheTagNotOnRootField {
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

fn validate_application_on_object_type(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    type_pos: &ObjectTypeDefinitionPosition,
    args: &CacheTagDirectiveArguments,
) -> Result<(), FederationError> {
    // validate the type is a resolvable entity
    let fed_spec = get_federation_spec_definition_from_subgraph(schema)?;
    let key_directive_def = fed_spec.key_directive_definition(schema)?;
    let type_def = type_pos.get(schema.schema())?;
    let is_resolvable = type_def
        .directives
        .get_all(&key_directive_def.name)
        .map(|directive_app| {
            let key_args = fed_spec.key_directive_arguments(directive_app)?;
            Ok::<_, FederationError>(key_args.resolvable)
        })
        .process_results(|mut iter| iter.any(|x| x))?;
    if !is_resolvable {
        let error =
            CacheTagValidationError::CacheTagEntityNotResolvable(type_pos.type_name.clone());
        errors.push(Message::new(schema, type_def, error));
        return Ok(());
    }

    // validate the arguments
    validate_args_on_object_type(schema, errors, type_pos, args)?;
    Ok(())
}

fn validate_args_on_field(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    field: &ObjectFieldDefinitionPosition,
    args: &CacheTagDirectiveArguments,
) -> Result<(), FederationError> {
    let field_def = field.get(schema.schema())?;
    let connect_spec = connect_spec_from_schema(schema.schema()).unwrap_or(DEFAULT_CONNECT_SPEC);
    let format = match StringTemplate::parse_with_spec(args.format, connect_spec) {
        Ok(format) => format,
        Err(err) => {
            errors.push(Message::new(schema, field_def, err.into()));
            return Ok(());
        }
    };
    let new_errors = format.expressions().filter_map(|expr| {
        expr.expression.if_named_else_path(
            |_named| {
                Some(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!("\"{}\"", expr.expression),
                })
            },
            |path| match path.variable_reference::<String>() {
                Some(var_ref) => {
                    // Check the namespace
                    if var_ref.namespace.namespace != "$args" {
                        return Some(
                            CacheTagValidationError::CacheTagInvalidFormatArgumentOnRootField,
                        );
                    }

                    // Check the selection
                    let fields = field_def
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.clone(), arg.ty.as_ref()))
                        .collect::<IndexMap<Name, &Type>>();
                    validate_args_selection(schema, None, &fields, &var_ref.selection).err()
                }
                None => None,
            },
        )
    });
    errors.extend(new_errors.map(|err| Message::new(schema, field_def, err)));
    Ok(())
}

/// parent_type_name: The name of the parent composite type; None if selection is a field argument.
fn validate_args_selection(
    schema: &FederationSchema,
    parent_type_name: Option<&Name>,
    fields: &IndexMap<Name, &Type>,
    selection: &SelectionTrie,
) -> Result<(), CacheTagValidationError> {
    // Check the format selection is just a single selection. The `StringTemplate` allows multiple
    // selections like `{$args { a b }}`, but cache tags don't support that.
    let num_selections = selection.iter().count();
    if num_selections != 1 {
        return Err(CacheTagValidationError::CacheTagInvalidFormat {
            message: format!(
                "invalid path element at \"{selection}\", which is not a single selection"
            ),
        });
    }
    for (key, sel) in selection.iter() {
        let name = Name::new(key).map_err(|_| CacheTagValidationError::CacheTagInvalidFormat {
            message: format!("invalid field selection name \"{key}\""),
        })?;
        let field =
            fields
                .get(&name)
                .ok_or_else(|| CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!("unknown field \"{name}\""),
                })?;
        if !is_fully_non_null(field) {
            if let Some(parent_type_name) = parent_type_name {
                return Err(CacheTagValidationError::CacheTagFormatNullableField {
                    field_name: name.clone(),
                    parent_type: parent_type_name.to_string(),
                });
            } else {
                return Err(CacheTagValidationError::CacheTagFormatNullableArgument {
                    arg_name: name.clone(),
                });
            }
        }
        let type_name = field.inner_named_type();
        let type_def = schema.get_type(type_name.clone())?;
        if !sel.is_leaf() {
            let type_def = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_def).map_err(
                |_| CacheTagValidationError::CacheTagInvalidFormat {
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

                    Ok::<_, CacheTagValidationError>((
                        field_pos.field_name().clone(),
                        &field_def.ty,
                    ))
                })
                .collect::<Result<IndexMap<_, _>, _>>()?;
            validate_args_selection(schema, Some(type_name), &next_fields, sel)?;
        } else {
            // A leaf field must not be a list.
            if field.is_list() {
                return Err(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!("invalid path ending at \"{name}\", which is a list type"),
                });
            }
            // A leaf field should have a scalar type.
            if !matches!(
                &type_def,
                TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_)
            ) {
                return Err(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!(
                        "invalid path ending at \"{name}\", which is not a scalar type or an enum"
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Similar to `Type::is_non_null`, but checks if the type is non-null at all nested levels of
/// lists.
fn is_fully_non_null(ty: &Type) -> bool {
    match ty {
        Type::Named(_) => false,
        Type::List(_) => false,
        Type::NonNullNamed(_) => true,
        Type::NonNullList(inner) => is_fully_non_null(inner),
    }
}

fn validate_args_on_object_type(
    schema: &FederationSchema,
    errors: &mut Vec<Message>,
    type_pos: &ObjectTypeDefinitionPosition,
    args: &CacheTagDirectiveArguments,
) -> Result<(), FederationError> {
    let type_def = type_pos.get(schema.schema())?;
    let connect_spec = connect_spec_from_schema(schema.schema()).unwrap_or(DEFAULT_CONNECT_SPEC);
    let format = match StringTemplate::parse_with_spec(args.format, connect_spec) {
        Ok(format) => format,
        Err(err) => {
            errors.push(Message::new(schema, type_def, err.into()));
            return Ok(());
        }
    };
    let res = format.expressions().filter_map(|expr| {
        expr.expression.if_named_else_path(
            |_named| {
                Some(Err(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!("\"{}\"", expr.expression),
                }))
            },
            |path| match path.variable_reference::<String>() {
                Some(var_ref) => {
                    // Check the namespace
                    if var_ref.namespace.namespace != "$key" {
                        return Some(Err(
                            CacheTagValidationError::CacheTagInvalidFormatArgumentOnEntity {
                                type_name: type_pos.type_name.clone(),
                                format: format.to_string(),
                            },
                        ));
                    }

                    // Build the selection set based on what's in the variable, so if it's
                    // $key.a.b it will generate { a { b } }
                    let mut selection_set = SelectionSet::new(type_pos.type_name.clone());
                    match build_selection_set(&mut selection_set, schema, &var_ref.selection) {
                        Ok(_) => Some(Ok(selection_set)),
                        Err(err) => Some(Err(err)),
                    }
                }
                None => None,
            },
        )
    });
    let mut format_selections = Vec::new();
    let mut has_error = false;
    for item in res {
        match item {
            Ok(sel) => format_selections.push(executable::FieldSet {
                selection_set: sel,
                sources: Default::default(),
            }),
            Err(err) => {
                has_error = true;
                errors.push(Message::new(schema, type_def, err));
            }
        }
    }
    if has_error {
        return Ok(());
    }

    // Check if all field sets coming from all collected StringTemplate ($key.a.b) from cacheTag
    // directives are each a subset of each entity keys
    let entity_key_field_sets = get_entity_key_field_sets(schema, type_pos)?;
    let is_correct = format_selections.into_iter().all(|format_field_set| {
        entity_key_field_sets.iter().all(|key_field_set| {
            crate::connectors::field_set_is_subset(&format_field_set, key_field_set)
        })
    });

    if !is_correct {
        let error = CacheTagValidationError::CacheTagInvalidFormatFieldSetOnEntity {
            type_name: type_pos.type_name.clone(),
            format: format.to_string(),
        };
        errors.push(Message::new(schema, type_def, error));
    }
    Ok(())
}

fn get_entity_key_field_sets(
    schema: &FederationSchema,
    type_pos: &ObjectTypeDefinitionPosition,
) -> Result<Vec<executable::FieldSet>, FederationError> {
    let fed_spec = get_federation_spec_definition_from_subgraph(schema)?;
    let key_directive_def = fed_spec.key_directive_definition(schema)?;
    let type_def = type_pos.get(schema.schema())?;
    type_def
        .directives
        .get_all(&key_directive_def.name)
        .map(|directive_app| {
            let key_args = fed_spec.key_directive_arguments(directive_app)?;
            executable::FieldSet::parse(
                Valid::assume_valid_ref(schema.schema()),
                type_pos.type_name.clone(),
                key_args.fields,
                "field_set",
            )
            .map_err(|err| internal_error!("cannot parse field set for entity keys: {err}"))
        })
        .process_results(|iter| iter.collect())
}

/// Build the selection set based on what's in the variable, so if it's $key.a.b it will generate { a { b } }
fn build_selection_set(
    selection_set: &mut SelectionSet,
    schema: &FederationSchema,
    selection: &SelectionTrie,
) -> Result<(), CacheTagValidationError> {
    // Check the format selection is just a single selection. The `StringTemplate` allows multiple
    // selections like `{$key { a b }}`, but cache tags don't support that.
    let num_selections = selection.iter().count();
    if num_selections != 1 {
        return Err(CacheTagValidationError::CacheTagInvalidFormat {
            message: format!(
                "invalid path element at \"{selection}\", which is not a single selection"
            ),
        });
    }
    for (key, sel) in selection.iter() {
        let name = Name::new(key).map_err(|_| CacheTagValidationError::CacheTagInvalidFormat {
            message: format!("invalid field selection name \"{key}\""),
        })?;
        let mut new_field = selection_set
            .new_field(schema.schema(), name.clone())
            .map_err(|_| CacheTagValidationError::CacheTagInvalidFormat {
                message: format!("cannot create selection set with \"{key}\""),
            })?;
        let new_field_type_def = schema
            .get_type(new_field.ty().inner_named_type().clone())
            .map_err(|_| CacheTagValidationError::CacheTagInvalidFormat {
                message: format!("invalid field selection name \"{key}\""),
            })?;

        if !is_fully_non_null(new_field.ty()) {
            return Err(CacheTagValidationError::CacheTagFormatNullableField {
                field_name: name.clone(),
                parent_type: selection_set.ty.to_string(),
            });
        }

        if !sel.is_leaf() {
            ObjectOrInterfaceTypeDefinitionPosition::try_from(new_field_type_def).map_err(
                |_| CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!(
                        "invalid path element \"{name}\", which is not an object or interface type"
                    ),
                },
            )?;
            build_selection_set(&mut new_field.selection_set, schema, sel)?;
        } else {
            // A leaf field must not be a list.
            if new_field.ty().is_list() {
                return Err(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!("invalid path ending at \"{name}\", which is a list type"),
                });
            }
            // A leaf field should have a scalar type.
            if !matches!(
                &new_field_type_def,
                TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_)
            ) {
                return Err(CacheTagValidationError::CacheTagInvalidFormat {
                    message: format!(
                        "invalid path ending at \"{name}\", which is not a scalar type or an enum"
                    ),
                });
            }
        }
        selection_set.push(new_field);
    }

    Ok(())
}

///////////////////////////////////////////////////////////////////////////////////////////////////
// Error messages

// Note: This is modeled after the Connectors' `Message` struct.
#[derive(Debug, Clone)]
pub struct Message {
    error: CacheTagValidationError,
    pub locations: Vec<Range<LineColumn>>,
}

impl Message {
    fn new<T>(
        schema: &FederationSchema,
        node: &apollo_compiler::Node<T>,
        error: CacheTagValidationError,
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
enum CacheTagValidationError {
    #[error("{error}")]
    FederationError { error: FederationError },
    #[error("cacheTag format is invalid: {message}")]
    CacheTagInvalidFormat { message: String },
    #[error(
        "error on field \"{field_name}\" on type \"{type_name}\": cacheTag can only apply on root fields"
    )]
    CacheTagNotOnRootField { type_name: Name, field_name: Name },
    #[error("cacheTag applied on root fields can only reference arguments in format using $args")]
    CacheTagInvalidFormatArgumentOnRootField,
    #[error("cacheTag applied on types can only reference arguments in format using $key")]
    CacheTagInvalidFormatArgumentOnEntity { type_name: Name, format: String },
    #[error(
        "Object \"{0}\" is not an entity. cacheTag can only apply on resolvable entities, object containing at least 1 @key directive and resolvable"
    )]
    CacheTagEntityNotResolvable(Name),
    #[error(
        "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"{format}\" on type \"{type_name}\""
    )]
    CacheTagInvalidFormatFieldSetOnEntity { type_name: Name, format: String },
    #[error("@cacheTag format references a nullable field \"{parent_type}.{field_name}\"")]
    CacheTagFormatNullableField {
        field_name: Name,
        parent_type: String,
    },
    #[error("@cacheTag format references a nullable argument \"{arg_name}\"")]
    CacheTagFormatNullableArgument { arg_name: Name },
}

impl CacheTagValidationError {
    fn code(&self) -> String {
        match self {
            // Special handling for FederationError
            CacheTagValidationError::FederationError { error } => match error {
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

impl From<FederationError> for CacheTagValidationError {
    fn from(error: FederationError) -> Self {
        CacheTagValidationError::FederationError { error }
    }
}

impl From<StringTemplateError> for CacheTagValidationError {
    fn from(error: StringTemplateError) -> Self {
        CacheTagValidationError::CacheTagInvalidFormat {
            message: error.to_string(),
        }
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
                         @cacheTag(format: "{ namedField }")
                         @cacheTag(format: "{$args}")
            {
                upc: String!
                name: String
            }

            type Query {
                topProducts(first: Int = 5): [Product]
                    @cacheTag(format: "{$this}")
                    @cacheTag(format: "{$key}")
            }
        "#;

        let subgraph = build_inner_expanded(SCHEMA, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
        assert_eq!(
            errors.iter().map(|e| e.code()).collect::<Vec<_>>(),
            vec![
                "CACHE_TAG_INVALID_FORMAT",
                "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ENTITY",
                "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ROOT_FIELD",
                "CACHE_TAG_INVALID_FORMAT_ARGUMENT_ON_ROOT_FIELD",
            ],
        );
    }

    #[track_caller]
    fn build_and_validate(schema: &str) {
        let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
        if !errors.is_empty() {
            for error in &errors {
                println!("Error: {}", error);
            }
        }
        assert!(errors.is_empty());
    }

    #[track_caller]
    fn build_for_errors(schema: &str) -> Vec<String> {
        let subgraph = build_inner_expanded(schema, BuildOption::AsFed2).unwrap();
        let mut errors = Vec::new();
        validate_cache_tag_directives(subgraph.schema(), &mut errors).unwrap();
        errors.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn test_valid_format_string() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc age")
                         @cacheTag(format: "product-{$key.upc}")
            {
                upc: String!
                age: Int!
                name: String
            }

            enum Country {
                BE
                FR
            }

            type Query {
                topProducts(first: Int! = 5): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args.first}")
                topProductsByCountry(first: Int! = 5, country: Country!): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args.first}-{$args.country}")
            }

            type Test @key(fields: "id country") @cacheTag(format: "test-{$key.id}-{$key.country}") {
                id: ID!
                country: Country!
            }
        "#;
        build_and_validate(SCHEMA);
    }

    #[test]
    fn test_invalid_format_string_nullable_args() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc name")
                         @cacheTag(format: "product-{$key.upc}-{$key.name}")
            {
                upc: String!
                name: String
            }

            type Query {
                topProducts(first: Int): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args.first}")
                productsByCountry(country: [String]!): [Product]
                    @cacheTag(format: "productsByCountry-{$args.country}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "@cacheTag format references a nullable field \"Product.name\"",
                "@cacheTag format references a nullable argument \"first\"",
                "@cacheTag format references a nullable argument \"country\"",
            ]
        );
    }

    #[test]
    fn test_invalid_format_string_list_args() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc names")
                         @cacheTag(format: "product-{$key.upc}-{$key.names}")
            {
                upc: String!
                names: [String!]!
            }

            type Query {
                topProducts(groups: [Int!]!): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args.groups}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "cacheTag format is invalid: invalid path ending at \"names\", which is a list type",
                "cacheTag format is invalid: invalid path ending at \"groups\", which is a list type",
            ]
        );
    }

    #[test]
    fn test_invalid_format_selection() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc")
                         @cacheTag(format: "{ namedField }")
                         @cacheTag(format: "{$args}")
            {
                upc: String!
                name: String
            }

            type Query {
                topProducts(first: Int = 5): [Product]
                    @cacheTag(format: "{$this}")
                    @cacheTag(format: "{$key}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "cacheTag format is invalid: \"namedField\"",
                "cacheTag applied on types can only reference arguments in format using $key",
                "cacheTag applied on root fields can only reference arguments in format using $args",
                "cacheTag applied on root fields can only reference arguments in format using $args",
            ]
        );
    }

    #[test]
    fn test_invalid_format_path_selection() {
        const SCHEMA: &str = r#"
            type Test {
                a: Int!
                b: Int!
                c: Int
            }

            type Product @key(fields: "upc test { a c }")
                         @cacheTag(format: "product-{$key.somethingElse}")
                         @cacheTag(format: "product-{$key.test}")
                         @cacheTag(format: "product-{$key.test.a}")
                         @cacheTag(format: "product-{$key.test.b}")
                         @cacheTag(format: "product-{$key.test.c}")
            {
                upc: String!
                test: Test!
                name: String
            }

            type Query {
                topProducts(first: Int = 5): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args { second }}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "cacheTag format is invalid: cannot create selection set with \"somethingElse\"",
                "cacheTag format is invalid: invalid path ending at \"test\", which is not a scalar type or an enum",
                "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"product-{$key.test.b}\" on type \"Product\"",
                "@cacheTag format references a nullable field \"Test.c\"",
                "cacheTag format is invalid: unknown field \"second\""
            ]
        );
    }

    #[test]
    fn test_invalid_format_string_multiple_selections() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc name")
                         @cacheTag(format: "product-{$key { upc name }}")
                         @cacheTag(format: "product-{$key {}}")
            {
                upc: String!
                name: String
            }

            type Query {
                topProducts(first: Int): [Product]
                    @cacheTag(format: "topProducts")
                    @cacheTag(format: "topProducts-{$args { first country }}")
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "cacheTag format is invalid: invalid path element at \"upc name\", which is not a single selection",
                "cacheTag format is invalid: invalid path element at \"\", which is not a single selection",
                "cacheTag format is invalid: invalid path element at \"first country\", which is not a single selection",
            ]
        );
    }

    #[test]
    fn test_valid_format_string_multiple_keys() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc x")
                         @key(fields: "upc y")
                         @cacheTag(format: "product-{$key.upc}")
            {
                upc: String!
                x: Int!
                y: Int!
                name: String
            }
        "#;
        build_and_validate(SCHEMA);
    }

    #[test]
    fn test_invalid_format_string_multiple_keys() {
        const SCHEMA: &str = r#"
            type Product @key(fields: "upc x")
                         @key(fields: "upc y")
                         @cacheTag(format: "product-{$key.x}")
            {
                upc: String!
                x: Int!
                y: Int!
                name: String
            }
        "#;
        assert_eq!(
            build_for_errors(SCHEMA),
            vec![
                "Each entity field referenced in a @cacheTag format (applied on entity type) must be a member of every @key field set. In other words, when there are multiple @key fields on the type, the referenced field(s) must be limited to their intersection. Bad cacheTag format \"product-{$key.x}\" on type \"Product\""
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
