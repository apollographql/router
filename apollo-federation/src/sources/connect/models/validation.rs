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

## @source

- name:
  - Valid characters
  - Unique

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

use apollo_compiler::schema::{Component, Directive};
use apollo_compiler::Schema;
use url::Url;

pub fn validate(schema: Schema) -> Vec<ValidationError> {
    let errors = schema
        .schema_definition
        .directives
        .iter()
        .filter_map(|directive| {
            (directive.name == "source")
                .then(|| validate_source(directive))
                .flatten()
        })
        .collect();
    errors
}

fn validate_source(directive: &Component<Directive>) -> Option<ValidationError> {
    let source_name = directive
        .arguments
        .iter()
        .find(|arg| arg.name == "name")?
        .value
        .as_str()?;
    directive
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
                    return Some(ValidationError::InvalidSourceUrl {
                        inner,
                        source_name: source_name.to_string(),
                    })
                }
                Ok(url) => url,
            };
            if url.scheme() != "http" && url.scheme() != "https" {
                return Some(ValidationError::InvalidSourceScheme {
                    scheme: url.scheme().to_string(),
                    source_name: source_name.to_string(),
                });
            }
            None
        })
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("baseURL argument for @source \"{source_name}\" was not a valid URL: {inner}")]
    InvalidSourceUrl {
        inner: url::ParseError,
        source_name: String,
    },
    #[error("baseURL argument for @source \"{source_name}\" be http or https, got {scheme}")]
    InvalidSourceScheme { scheme: String, source_name: String },
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
        assert!(matches!(
            &errors[0],
            ValidationError::InvalidSourceUrl { .. }
        ));
    }

    #[test]
    fn incorrect_scheme() {
        let schema = include_str!("test_data/invalid_source_url_scheme.graphql");
        let schema = Schema::parse(schema, "test.graphql").unwrap();
        let errors = validate(schema);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            ValidationError::InvalidSourceScheme { .. }
        ));
    }
}
