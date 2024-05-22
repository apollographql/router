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
- If source:
  - Matches a @source
  - Transport is the same as the @source
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
use std::error::Error;
use std::fmt::Display;

use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::Node;
use apollo_compiler::NodeLocation;
use apollo_compiler::Schema;
use url::Url;

use crate::link::Link;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;
use crate::sources::connect::ConnectSpecDefinition;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<ValidationError> {
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
    let source_directive_name = ConnectSpecDefinition::source_directive_name(&link);
    let source_directives: Vec<SourceDirective> = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == source_directive_name)
        .map(validate_source)
        .collect();

    let mut errors = Vec::new();
    let mut names = HashMap::new();
    for directive in source_directives {
        errors.extend(directive.errors);
        match directive.name.into_value_or_error() {
            Err(error) => errors.push(error),
            Ok(name) => names
                .entry(name)
                .or_insert_with(Vec::new)
                .extend(directive.directive.node.location().into_iter()),
        }
    }
    for (name, locations) in names {
        if locations.len() > 1 {
            errors.push(ValidationError {
                message: format!("every @{source_directive_name} name must be unique; found duplicate source name {name}"),
                code: ErrorCode::DuplicateSourceName,
                locations,
            });
        }
    }
    errors
}

fn validate_source(directive: &Component<Directive>) -> SourceDirective {
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
                    return Some(ValidationError {
                        code: ErrorCode::SourceUrl,
                        message: format!(
                            "baseURL argument for {name} was not a valid URL: {inner}"
                        ),
                        locations: arg.location().into_iter().collect(),
                    })
                }
                Ok(url) => url,
            };
            let scheme = url.scheme();
            if scheme != "http" && scheme != "https" {
                return Some(ValidationError {
                    code: ErrorCode::SourceScheme,
                    message: format!(
                        "baseURL argument for {name} must be http or https, got {scheme}"
                    ),
                    locations: arg.value.location().into_iter().collect(),
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
    errors: Vec<ValidationError>,
    directive: Component<Directive>,
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

    pub fn into_value_or_error(self) -> Result<Node<Value>, ValidationError> {
        match self {
            Self::Valid { value, ..} => Ok(value),
            Self::Invalid {
                value,
                directive_name,
            } => Err(ValidationError {
                message: format!("invalid characters in @{directive_name} name {value}, only alphanumeric and underscores are allowed"),
                code: ErrorCode::InvalidSourceName,
                locations: value.location().into_iter().collect(),
            }),
            Self::Empty { directive_name, value } => {
                Err(ValidationError {
                    code: ErrorCode::EmptySourceName,
                    message: format!("name argument to @{directive_name} can't be empty"),
                    locations: value.location().into_iter().collect(),
                })
            }
            Self::Missing { directive_name, ast_node } => Err(ValidationError {
                code: ErrorCode::GraphQLError,
                message: format!("missing name argument to @{directive_name}"),
                locations: ast_node.location().into_iter().collect()
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

#[derive(Debug)]
pub struct ValidationError {
    pub code: ErrorCode,
    pub message: String,
    pub locations: Vec<NodeLocation>,
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for ValidationError {}

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
        assert_snapshot!(errors[0].to_string());
    }

    #[test]
    fn incorrect_scheme() {
        let schema = include_str!("test_data/invalid_source_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string());
    }

    #[test]
    fn invalid_chars_in_source_name() {
        let schema = include_str!("test_data/invalid_chars_in_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string());
    }

    #[test]
    fn empty_source_name() {
        let schema = include_str!("test_data/empty_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string());
    }

    #[test]
    fn duplicate_source_name() {
        let schema = include_str!("test_data/duplicate_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string());
    }

    #[test]
    fn multiple_errors() {
        let schema = include_str!("test_data/multiple_errors.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 2);
        assert_snapshot!(errors[0].to_string());
        assert_snapshot!(errors[1].to_string());
    }

    #[test]
    fn source_directive_rename() {
        let schema = include_str!("test_data/source_directive_rename.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert_snapshot!(errors[0].to_string());
    }
}
