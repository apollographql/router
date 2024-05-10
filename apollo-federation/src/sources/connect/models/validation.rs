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

*/

use std::fmt::Display;

use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::Schema;
use itertools::Itertools;
use url::Url;

/// Validate the connectors-related directives `@source` and `@connect`.
///
/// This function attempts to collect as many validation errors as possible, so it does not bail
/// out as soon as it encounters one.
pub fn validate(schema: Schema) -> Vec<ValidationError> {
    let mut source_names = Vec::new();
    let errors = schema
        .schema_definition
        .directives
        .iter()
        .filter_map(|directive| {
            if directive.name == "source" {
                match validate_source(directive) {
                    Ok(source_directive) => {
                        if let SourceName::Valid(name) = source_directive.name {
                            source_names.push(name);
                        }
                        (!source_directive.errors.is_empty()).then_some(source_directive.errors)
                    }
                    Err(error) => Some(vec![error]),
                }
            } else {
                None
            }
        })
        .flatten()
        .collect_vec();
    // Check for duplicate source names
    source_names
        .into_iter()
        .counts()
        .into_iter()
        .filter_map(|(name, count)| {
            (count > 1).then_some(ValidationError::DuplicateSourceName(name))
        })
        .chain(errors)
        .collect()
}

fn validate_source(directive: &Component<Directive>) -> Result<SourceDirective, ValidationError> {
    let name = SourceName::try_from(directive)?;
    let url_error = directive
        .arguments
        .iter()
        .find(|arg| arg.name == "http")
        .and_then(|arg| {
            let value = arg
                .value
                .as_object()?
                .iter()
                .find_map(|(name, value)| (name == "baseURL").then_some(value))?
                .as_str()?;
            let url = match Url::parse(value) {
                Err(inner) => {
                    return Some(ValidationError::SourceUrl {
                        inner,
                        source_name: name.clone(),
                    })
                }
                Ok(url) => url,
            };
            if url.scheme() != "http" && url.scheme() != "https" {
                return Some(ValidationError::SourceScheme {
                    scheme: url.scheme().to_string(),
                    source_name: name.clone(),
                });
            }
            None
        });
    let mut errors = Vec::new();
    if matches!(name, SourceName::Invalid(_)) {
        errors.push(ValidationError::InvalidSourceName(name.clone()));
    }
    if let Some(url_error) = url_error {
        errors.push(url_error);
    }
    Ok(SourceDirective { name, errors })
}

/// A `@source` directive along with any errors related to it.
struct SourceDirective {
    name: SourceName,
    errors: Vec<ValidationError>,
}

/// The `name` argument of a `@source` directive.
#[derive(Clone, Debug)]
pub enum SourceName {
    /// A perfectly reasonable source name.
    Valid(String),
    /// Contains invalid characters, so it will have to be renamed. This means certain checks
    /// (like uniqueness) should be skipped. However, we have _a_ name, so _other_ checks on the
    /// `@source` directive can continue.
    Invalid(String),
}

impl From<SourceName> for String {
    fn from(source_name: SourceName) -> String {
        match source_name {
            SourceName::Valid(name) | SourceName::Invalid(name) => name,
        }
    }
}

impl TryFrom<&Component<Directive>> for SourceName {
    type Error = ValidationError;

    fn try_from(directive: &Component<Directive>) -> Result<Self, Self::Error> {
        let str_value = directive
            .arguments
            .iter()
            .find(|arg| arg.name == "name")
            .ok_or(ValidationError::MissingSourceName)?
            .value
            .as_str()
            .ok_or(ValidationError::SourceNameType)?;
        if str_value.is_empty() {
            Err(ValidationError::EmptySourceName)
        } else if str_value.chars().all(|c| c.is_alphanumeric() || c == '_') {
            Ok(Self::Valid(str_value.to_string()))
        } else {
            Ok(Self::Invalid(str_value.to_string()))
        }
    }
}

impl Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid(name) | Self::Invalid(name) => write!(f, "{}", name),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("baseURL argument for @source \"{source_name}\" was not a valid URL: {inner}")]
    SourceUrl {
        inner: url::ParseError,
        source_name: SourceName,
    },
    #[error("baseURL argument for @source \"{source_name}\" be http or https, got {scheme}")]
    SourceScheme {
        scheme: String,
        source_name: SourceName,
    },
    #[error("missing name argument for a @source directive")]
    MissingSourceName,
    #[error("name argument for a @source directive must be a string")]
    SourceNameType,
    #[error("name argument to @source can't be empty")]
    EmptySourceName,
    #[error(
        "invalid characters in source name {0}, only alphanumeric and underscores are allowed"
    )]
    InvalidSourceName(SourceName),
    /// Intentionally a String so we remember to only check valid source names
    #[error("every @source name must be unique; found duplicate source name {0}")]
    DuplicateSourceName(String),
}
#[cfg(test)]
mod test_validate_source {
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
        assert!(matches!(
            &errors[0],
            ValidationError::InvalidSourceName(SourceName::Invalid(_))
        ));
    }

    #[test]
    fn empty_source_name() {
        let schema = include_str!("test_data/empty_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ValidationError::EmptySourceName));
    }

    #[test]
    fn duplicate_source_name() {
        let schema = include_str!("test_data/duplicate_source_name.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            ValidationError::DuplicateSourceName(_)
        ));
    }

    #[test]
    fn multiple_errors() {
        let schema = include_str!("test_data/multiple_errors.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 2);
        assert!(matches!(
            &errors[0],
            ValidationError::InvalidSourceName { .. }
        ));
        assert!(matches!(&errors[1], ValidationError::SourceScheme { .. }));
    }
}
