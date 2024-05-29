//! Validation of the `@source` and `@connect` directives.

// No panics allowed in this module
#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unimplemented,
        clippy::todo
    )
)]

/* TODO:

## @connect

- Disallowed on Subscription fields
- http:
  - Has one path (GET, POST, etc)
  - URL path template valid syntax
  - body is valid syntax
- selection
  - Valid syntax
- If no source:
  - http:
    - Path is a fully qualified URL (plus a URL path template)
- If entity: true
  - Must be on Query type
  - Field arguments must match fields in type (and @key if present)
  - Required for types with @key (eventually with @finder)

## HTTPHeaderMapping

- name: is unique
- name: is a valid header name
- as: is a valid header name
- value: is a list of valid header values
- as: and value: cannot both be present

## Output selection

- Empty composite selection
- Leaf with selections

## Input selections (URL path templates and bodies)

- Missing $args
- Missing $this

## @link

- Rename @connect import

*/

use std::collections::HashMap;
use std::fmt::Display;

use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::NodeLocation;
use apollo_compiler::Schema;
use apollo_compiler::SourceMap;
use itertools::Itertools;
use url::Url;

use crate::link::Link;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::ConnectSpecDefinition;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<Error> {
    let connect_identity = ConnectSpecDefinition::identity();
    let link = schema
        .schema_definition
        .directives
        .iter()
        .find_map(|directive| {
            let link = Link::from_directive_application(directive).ok()?;
            if link.url.identity == connect_identity {
                Some(link)
            } else {
                None
            }
        });
    let Some(link) = link else {
        return vec![]; // There are no connectors-related directives to validate
    };
    let source_map = schema.sources;
    let source_directive_name = ConnectSpecDefinition::source_directive_name(&link);
    let connect_directive_name = ConnectSpecDefinition::connect_directive_name(&link);
    let source_directives: Vec<SourceDirective> = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == source_directive_name)
        .map(|directive| validate_source(directive, &source_map))
        .collect();

    let mut errors = Vec::new();
    let mut valid_source_names = HashMap::new();
    let all_source_names = source_directives
        .iter()
        .map(|directive| directive.name.clone())
        .collect_vec();
    for directive in source_directives {
        errors.extend(directive.errors);
        match directive.name.into_value_or_error(&source_map) {
            Err(error) => errors.push(error),
            Ok(name) => valid_source_names
                .entry(name)
                .or_insert_with(Vec::new)
                .extend(GraphQLLocation::from_node(
                    directive.directive.node.location(),
                    &source_map,
                )),
        }
    }
    for (name, locations) in valid_source_names {
        if locations.len() > 1 {
            errors.push(Error {
                message: format!("every @{source_directive_name} name must be unique; found duplicate source name {name}"),
                code: ErrorCode::DuplicateSourceName,
                locations,
            });
        }
    }
    let connect_errors = schema
        .types
        .values()
        .filter_map(|extended_type| match extended_type {
            ExtendedType::Object(object) => Some(object),
            _ => None,
        })
        .flat_map(|object| {
            validate_object(
                object,
                &source_map,
                &connect_directive_name,
                &source_directive_name,
                &all_source_names,
            )
        });
    errors.extend(connect_errors);
    errors
}

fn validate_source(directive: &Component<Directive>, sources: &SourceMap) -> SourceDirective {
    let name = SourceName::from_directive(directive);
    let url_error = directive
        .arguments
        .iter()
        .find(|arg| arg.name == SOURCE_HTTP_ARGUMENT_NAME)
        .and_then(|arg| {
            let value = arg
                .value
                .as_object()?
                .iter()
                .find_map(|(name, value)| {
                    (*name == SOURCE_BASE_URL_ARGUMENT_NAME).then_some(value)
                })?
                .as_str()?;
            let url = match Url::parse(value) {
                Err(inner) => {
                    return Some(Error {
                        code: ErrorCode::SourceUrl,
                        message: format!(
                            "baseURL argument for {name} was not a valid URL: {inner}"
                        ),
                        locations: GraphQLLocation::from_node(arg.location(), sources)
                            .into_iter()
                            .collect(),
                    })
                }
                Ok(url) => url,
            };
            let scheme = url.scheme();
            if scheme != "http" && scheme != "https" {
                return Some(Error {
                    code: ErrorCode::SourceScheme,
                    message: format!(
                        "baseURL argument for {name} must be http or https, got {scheme}"
                    ),
                    locations: GraphQLLocation::from_node(arg.value.location(), sources)
                        .into_iter()
                        .collect(),
                });
            }
            None
        });
    let mut errors = Vec::new();
    if let Some(url_error) = url_error {
        errors.push(url_error);
    }
    SourceDirective {
        name,
        errors,
        directive: directive.clone(),
    }
}

/// A `@source` directive along with any errors related to it.
struct SourceDirective {
    name: SourceName,
    errors: Vec<Error>,
    directive: Component<Directive>,
}

fn validate_object(
    object: &Node<ObjectType>,
    sources: &SourceMap,
    connect_directive_name: &Name,
    source_directive_name: &Name,
    source_names: &[SourceName],
) -> Vec<Error> {
    let fields = object.fields.values();
    let mut errors = Vec::new();
    for field in fields {
        let Some(connect_directive) = field
            .directives
            .iter()
            .find(|directive| directive.name == *connect_directive_name)
        else {
            // TODO: return errors when a field is not resolvable by some combination of `@connect`
            continue;
        };
        let Some((source_name, source_name_value)) =
            connect_directive.arguments.iter().find_map(|arg| {
                if arg.name == CONNECT_SOURCE_ARGUMENT_NAME {
                    arg.value.as_str().map(|value| (arg, value))
                } else {
                    None
                }
            })
        else {
            // TODO: handle direct http connects, not just named sources
            continue;
        };

        if source_names.iter().all(|name| name != source_name_value) {
            // TODO: Pick a suggestion that's not just the first defined source
            let message = if let Some(first_source_name) = source_names.first() {
                format!(
                        "The value `@{connect_directive_name}({SOURCE_NAME_ARGUMENT}: \"{source_name_value}\")` on  `{object_name}.{field_name}` does not match any defined sources. Did you mean {first_source_name}?",
                        object_name = object.name,
                        field_name = field.name,
                    )
            } else {
                format!(
                        "The value `@{connect_directive_name}({SOURCE_NAME_ARGUMENT}:) on `{object_name}.{field_name}` specifies a source, but none are defined. Try adding @{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}: \"{source_name_value}\") to the schema.",
                        object_name = object.name,
                        field_name = field.name,
                        source_directive_name = source_directive_name,
                        source_name_value = source_name_value,
                    )
            };
            errors.push(Error {
                code: ErrorCode::GraphQLError,
                message,
                locations: GraphQLLocation::from_node(source_name.location(), sources)
                    .into_iter()
                    .collect(),
            });
        }
    }
    errors
}

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug)]
enum SourceName {
    /// A perfectly reasonable source name.
    Valid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// Contains invalid characters, so it will have to be renamed. This means certain checks
    /// (like uniqueness) should be skipped. However, we have _a_ name, so _other_ checks on the
    /// `@source` directive can continue.
    Invalid {
        value: Node<Value>,
        directive_name: DirectiveName,
    },
    /// The name was an empty string
    Empty {
        directive_name: DirectiveName,
        value: Node<Value>,
    },
    /// No `name` argument was defined
    Missing {
        directive_name: DirectiveName,
        ast_node: Node<Directive>,
    },
}

type DirectiveName = String;

impl SourceName {
    fn from_directive(directive: &Component<Directive>) -> Self {
        let directive_name = directive.name.to_string();
        let Some(arg) = directive
            .arguments
            .iter()
            .find(|arg| arg.name == SOURCE_NAME_ARGUMENT_NAME)
        else {
            return Self::Missing {
                directive_name,
                ast_node: directive.node.clone(),
            };
        };
        let Some(str_value) = arg.value.as_str() else {
            return Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            };
        };
        if str_value.is_empty() {
            Self::Empty {
                directive_name,
                value: arg.value.clone(),
            }
        } else if str_value.chars().all(|c| c.is_alphanumeric() || c == '_') {
            Self::Valid {
                value: arg.value.clone(),
                directive_name,
            }
        } else {
            Self::Invalid {
                value: arg.value.clone(),
                directive_name,
            }
        }
    }

    pub fn into_value_or_error(self, sources: &SourceMap) -> Result<Node<Value>, Error> {
        match self {
            Self::Valid { value, ..} => Ok(value),
            Self::Invalid {
                value,
                directive_name,
            } => Err(Error {
                message: format!("invalid characters in @{directive_name} name {value}, only alphanumeric and underscores are allowed"),
                code: ErrorCode::InvalidSourceName,
                locations: GraphQLLocation::from_node(value.location(), sources).into_iter().collect(),
            }),
            Self::Empty { directive_name, value } => {
                Err(Error {
                    code: ErrorCode::EmptySourceName,
                    message: format!("name argument to @{directive_name} can't be empty"),
                    locations: GraphQLLocation::from_node(value.location(), sources).into_iter().collect(),
                })
            }
            Self::Missing { directive_name, ast_node } => Err(Error {
                code: ErrorCode::GraphQLError,
                message: format!("missing name argument to @{directive_name}"),
                locations: GraphQLLocation::from_node(ast_node.location(), sources).into_iter().collect()
            }),
        }
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid {
                value,
                directive_name,
            }
            | Self::Invalid {
                value,
                directive_name,
            } => write!(f, "@{directive_name} {value}"),
            Self::Empty { directive_name, .. } | Self::Missing { directive_name, .. } => {
                write!(f, "unnamed @{directive_name}")
            }
        }
    }
}

impl PartialEq<str> for SourceName {
    fn eq(&self, other: &str) -> bool {
        match self {
            Self::Valid { value, .. } | Self::Invalid { value, .. } => {
                value.as_str().is_some_and(|str_val| str_val == other)
            }
            Self::Empty { .. } | Self::Missing { .. } => other.is_empty(),
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub locations: Vec<GraphQLLocation>,
}

#[derive(Debug)]
pub struct GraphQLLocation {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

impl GraphQLLocation {
    // TODO: This is a ripoff of GraphQLLocation::from_node in apollo_compiler, contribute it back
    fn from_node(node: Option<NodeLocation>, sources: &SourceMap) -> Option<Self> {
        let node = node?;
        let source = sources.get(&node.file_id())?;
        let (start_line, start_column) = source
            .get_line_column(node.offset())
            .map(|(line, column)| (line + 1, column + 1))?;
        let (end_line, end_column) = source
            .get_line_column(node.end_offset())
            .map(|(line, column)| (line + 1, column + 1))?;
        Some(Self {
            start_line,
            start_column,
            end_line,
            end_column,
        })
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
pub enum ErrorCode {
    GraphQLError,
    DuplicateSourceName,
    InvalidSourceName,
    EmptySourceName,
    SourceUrl,
    SourceScheme,
}

#[cfg(test)]
mod test_validate_source {
    use insta::assert_snapshot;

    use super::*;

    #[test]
    fn unparseable_url() {
        let schema = include_str!("test_data/invalid_source_url.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"baseURL argument for @source "v1" was not a valid URL: relative URL without a base"###);
    }

    #[test]
    fn incorrect_scheme() {
        let schema = include_str!("test_data/invalid_source_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"baseURL argument for @source "v1" must be http or https, got file"###);
    }

    #[test]
    fn invalid_chars_in_source_name() {
        let schema = include_str!("test_data/invalid_chars_in_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"invalid characters in @source name "u$ers", only alphanumeric and underscores are allowed"###);
    }

    #[test]
    fn empty_source_name() {
        let schema = include_str!("test_data/empty_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @"name argument to @source can't be empty");
    }

    #[test]
    fn duplicate_source_name() {
        let schema = include_str!("test_data/duplicate_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"every @source name must be unique; found duplicate source name "v1""###);
    }

    #[test]
    fn multiple_errors() {
        let schema = include_str!("test_data/multiple_errors.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 2);
        assert_snapshot!(errors[0].to_string(), @r###"baseURL argument for @source "u$ers" must be http or https, got ftp"###);
        assert_snapshot!(errors[1].to_string(), @r###"invalid characters in @source name "u$ers", only alphanumeric and underscores are allowed"###);
    }

    #[test]
    fn source_directive_rename() {
        let schema = include_str!("test_data/source_directive_rename.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"baseURL argument for @api "users" was not a valid URL: relative URL without a base"###);
    }

    #[test]
    fn connect_source_name_mismatch() {
        let schema = include_str!("test_data/connect_source_name_mismatch.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"the source name "v1" for `Query.resources` does not match any defined sources. Did you mean @source "v2"?"###);
    }

    #[test]
    fn connect_source_undefined() {
        let schema = include_str!("test_data/connect_source_undefined.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string(), @r###"`Query.resources` specifies a source, but none are defined. Try adding @source(name: "v1") to the schema."###);
    }
}
