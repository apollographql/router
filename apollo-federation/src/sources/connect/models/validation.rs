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

use std::fmt::Display;

use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::Schema;
use itertools::Itertools;
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
        return vec![ValidationError::GraphQLError(
            "No connect spec definition. If you see this error, it's a bug, please report it."
                .to_string(),
        )];
    };
    let source_directive_name = ConnectSpecDefinition::source_directive_name(&link);
    let (source_names, errors): (Vec<Option<String>>, Vec<Vec<ValidationError>>) = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == source_directive_name)
        .map(|directive| {
            let SourceDirective { name, errors } = validate_source(directive);
            (name, errors)
        })
        .unzip();

    let errors = errors.into_iter().flatten().collect_vec();
    // Check for duplicate source names
    let source_names = source_names.into_iter().flatten().counts();

    source_names
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| ValidationError::DuplicateSourceName {
            source_name: name,
            directive_name: source_directive_name.to_string(),
        })
        .chain(errors)
        .collect()
}

fn validate_source(directive: &Component<Directive>) -> SourceDirective {
    let source_name = SourceName::from_directive(directive);
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
                    return Some(ValidationError::SourceUrl {
                        inner,
                        source_name: source_name.clone(),
                    })
                }
                Ok(url) => url,
            };
            if url.scheme() != "http" && url.scheme() != "https" {
                return Some(ValidationError::SourceScheme {
                    scheme: url.scheme().to_string(),
                    source_name: source_name.clone(),
                });
            }
            None
        });
    let mut errors = Vec::new();
    if let Some(url_error) = url_error {
        errors.push(url_error);
    }
    let name = match source_name.into_value_or_error() {
        Ok(name) => Some(name),
        Err(error) => {
            errors.push(error);
            None
        }
    };
    SourceDirective { name, errors }
}

/// A `@source` directive along with any errors related to it.
struct SourceDirective {
    name: Option<String>,
    errors: Vec<ValidationError>,
}

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug)]
pub enum SourceName {
    /// A perfectly reasonable source name.
    Valid {
        name: String,
        directive_name: DirectiveName,
    },
    /// Contains invalid characters, so it will have to be renamed. This means certain checks
    /// (like uniqueness) should be skipped. However, we have _a_ name, so _other_ checks on the
    /// `@source` directive can continue.
    Invalid {
        name: String,
        directive_name: DirectiveName,
    },
    /// The name was an empty string
    Empty { directive_name: DirectiveName },
    /// No `name` argument was defined
    Missing { directive_name: DirectiveName },
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
            return Self::Missing { directive_name };
        };
        let Some(str_value) = arg.value.as_str() else {
            return Self::Invalid {
                name: arg.value.to_string(),
                directive_name,
            };
        };
        if str_value.is_empty() {
            Self::Empty { directive_name }
        } else if str_value.chars().all(|c| c.is_alphanumeric() || c == '_') {
            Self::Valid {
                name: str_value.to_string(),
                directive_name,
            }
        } else {
            Self::Invalid {
                name: str_value.to_string(),
                directive_name,
            }
        }
    }

    fn into_value_or_error(self) -> Result<String, ValidationError> {
        match self {
            Self::Valid { name, .. } => Ok(name),
            Self::Invalid {
                name,
                directive_name,
            } => Err(ValidationError::InvalidSourceName {
                name,
                directive_name,
            }),
            Self::Empty { directive_name } => {
                Err(ValidationError::EmptySourceName { directive_name })
            }
            Self::Missing { directive_name } => Err(ValidationError::GraphQLError(format!(
                "missing name argument to @{directive_name}"
            ))),
        }
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid {
                name,
                directive_name,
            }
            | Self::Invalid {
                name,
                directive_name,
            } => write!(f, "@{directive_name} \"{name}\""),
            Self::Empty { directive_name } | Self::Missing { directive_name } => {
                write!(f, "unnamed @{directive_name}")
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("invalid GraphQL: {0}")]
    GraphQLError(String),
    #[error("baseURL argument for {source_name} was not a valid URL: {inner}")]
    SourceUrl {
        inner: url::ParseError,
        source_name: SourceName,
    },
    #[error("baseURL argument for {source_name} must be http or https, got {scheme}")]
    SourceScheme {
        scheme: String,
        source_name: SourceName,
    },
    #[error("name argument to @{directive_name} can't be empty")]
    EmptySourceName { directive_name: DirectiveName },
    #[error(
        "invalid characters in @{directive_name} name {name}, only alphanumeric and underscores are allowed"
    )]
    InvalidSourceName {
        name: String,
        directive_name: DirectiveName,
    },
    #[error(
        "every @{directive_name} name must be unique; found duplicate source name {source_name}"
    )]
    DuplicateSourceName {
        directive_name: DirectiveName,
        source_name: String,
    },
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
        assert!(matches!(&errors[0], ValidationError::SourceUrl { .. }));
    }

    #[test]
    fn incorrect_scheme() {
        let schema = include_str!("test_data/invalid_source_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ValidationError::SourceScheme { .. }));
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
        assert!(matches!(
            &errors[1],
            ValidationError::InvalidSourceName { .. }
        ));
        assert!(matches!(&errors[0], ValidationError::SourceScheme { .. }));
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
