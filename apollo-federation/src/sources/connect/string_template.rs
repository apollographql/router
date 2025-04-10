//! A [`StringTemplate`] is a string containing one or more [`Expression`]s.
//! These are used in connector URIs and headers.
//!
//! Parsing (this module) is done by both the router at startup and composition. Validation
//! (in [`crate::sources::connect::validation`]) is done only by composition.

use std::fmt::Display;
use std::fmt::Write;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use http::Uri;
use itertools::Itertools;
use percent_encoding::AsciiSet;
use percent_encoding::CONTROLS;
use percent_encoding::NON_ALPHANUMERIC;
use percent_encoding::utf8_percent_encode;
use serde_json_bytes::Value;

use crate::sources::connect::JSONSelection;

/// https://url.spec.whatwg.org/#fragment-percent-encode-set
const FRAGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');

/// https://url.spec.whatwg.org/#path-percent-encode-set
const PATH_SEGMENT: &AsciiSet = &FRAGMENT
    .add(b'#')
    .add(b'?')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%')
    .add(b'\\');

// https://url.spec.whatwg.org/#query-state
const QUERY: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'\'');

const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A parsed string template, containing a series of [`Part`]s.
///
/// The `RenderTo` changes some behaviors depending on the type:
/// - `String` is the most basic string template
/// - [`HeaderValue`] does some validation that the output is a valid header value and can be
///   more efficient in how it uses bytes.
/// - [`Uri`] applies percent encoding as the result is built
#[derive(Clone, Debug)]
pub struct StringTemplate {
    pub(crate) parts: Vec<Part>,
}

impl FromStr for StringTemplate {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        let mut offset = 0;
        let mut chars = input.chars().peekable();
        let mut parts = Vec::new();
        while let Some(next) = chars.peek() {
            if *next == '{' {
                let mut braces_count = 0; // Ignore braces within JSONSelection
                let expression = chars
                    .by_ref()
                    .skip(1)
                    .take_while(|c| {
                        if *c == '{' {
                            braces_count += 1;
                        } else if *c == '}' {
                            braces_count -= 1;
                        }
                        braces_count >= 0
                    })
                    .collect::<String>();
                if braces_count >= 0 {
                    return Err(Error {
                        message: "Invalid expression, missing closing }".into(),
                        location: offset..input.len(),
                    });
                }
                offset += 1; // Account for opening brace
                let parsed = JSONSelection::parse(&expression).map_err(|err| {
                    let start_of_parse_error = offset + err.offset;
                    Error {
                        message: err.message,
                        location: start_of_parse_error..(offset + expression.len()),
                    }
                })?;
                parts.push(Part::Expression(Expression {
                    expression: parsed,
                    location: offset..(offset + expression.len()),
                }));
                offset += expression.len() + 1; // Account for closing brace
            } else {
                let value = chars
                    .by_ref()
                    .peeking_take_while(|c| *c != '{')
                    .collect::<String>();
                let len = value.len();
                parts.push(Part::Constant(Constant {
                    value,
                    location: offset..offset + len,
                }));
                offset += len;
            }
        }
        Ok(StringTemplate { parts })
    }
}

impl StringTemplate {
    /// Get all the dynamic [`Expression`] pieces of the template for validation. If interpolating
    /// the entire template, use [`Self::interpolate`] instead.
    pub(crate) fn expressions(&self) -> impl Iterator<Item = &Expression> {
        self.parts.iter().filter_map(|part| {
            if let Part::Expression(expression) = part {
                Some(expression)
            } else {
                None
            }
        })
    }
}

impl StringTemplate {
    /// Interpolation for when the constant type is a string. This can't be implemented for
    /// arbitrary generic types, so non-string consumers (headers) implement this themselves with
    /// any additional validations/transformations they need.
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<String, Error> {
        // TODO: accumulate the result instead of allocating a new string for each part, when possible
        self.parts
            .iter()
            .map(|part| part.interpolate(vars))
            .collect()
    }

    /// Interpolate the expression as a URI, percent-encoding parts as needed.
    pub fn interpolate_uri(&self, vars: &IndexMap<String, Value>) -> Result<Uri, Error> {
        let mut result = String::new();
        for part in &self.parts {
            let new_part = part.interpolate(vars)?;

            let encoding = if let Part::Constant(_) = part {
                // We still need to %encode _some_ special characters for literals, but not all of them.
                // TODO: parse each constant into components and properly encode additional symbols
                //  for example, a literal domain can't accept the same characters as a path
                FRAGMENT
            } else if result.contains('#') {
                FRAGMENT
            } else if result.contains('?') {
                UNRESERVED
            } else {
                PATH_SEGMENT
            };
            write!(
                &mut result,
                "{}",
                utf8_percent_encode(new_part.as_str(), encoding)
            )
            .map_err(|err| Error {
                // In practice this should never fail, but let's not panic just in case
                message: format!("Error writing URI: {}", err),
                location: part.location(),
            })?;
        }
        Uri::from_str(&result).map_err(|err| Error {
            message: format!("Invalid URI: {}", err),
            location: 0..result.len(),
        })
    }
}

/// Expressions should be written the same as they were originally, even though we don't keep the
/// original source around. So constants are written as-is and expressions are surrounded with `{ }`.
impl Display for StringTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in &self.parts {
            match part {
                Part::Constant(Constant { value, .. }) => write!(f, "{}", value)?,
                Part::Expression(Expression { expression, .. }) => write!(f, "{{{}}}", expression)?,
            }
        }
        Ok(())
    }
}

/// A general-purpose error type which includes both a description of the problem and the offset span
/// within the original expression where the problem occurred. Used for both parsing and interpolation.
#[derive(Debug, PartialEq)]
pub struct Error {
    /// A human-readable description of the issue.
    pub message: String,
    /// The string offsets to the original [`StringTemplate`] (not just the part) where the issue
    /// occurred. As per usual, the end of the range is exclusive.
    pub(crate) location: Range<usize>,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

/// One piece of a [`StringTemplate`]
#[derive(Clone, Debug)]
pub(crate) enum Part {
    /// A constant string literal—the piece of a [`StringTemplate`] _not_ in `{ }`
    Constant(Constant),
    /// A dynamic piece of a [`StringTemplate`], which came from inside `{ }` originally.
    Expression(Expression),
}

impl Part {
    /// Get the original location of the part from the string which was parsed to form the
    /// [`StringTemplate`].
    fn location(&self) -> Range<usize> {
        match self {
            Self::Constant(c) => c.location.clone(),
            Self::Expression(e) => e.location.clone(),
        }
    }
}

impl Part {
    /// Evaluate the expression of the part (if any) and return the resulting String.
    ///
    /// # Errors
    ///
    /// If the expression evaluates to an array or object.
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<String, Error> {
        match self {
            Part::Constant(Constant { value, .. }) => Ok(value.clone()),
            Part::Expression(Expression { expression, .. }) => {
                // TODO: do something with the ApplyTo errors
                let (value, _errs) = expression.apply_with_vars(&Value::Null, vars);

                match value.unwrap_or(Value::Null) {
                    Value::Null => Ok(String::new()),
                    Value::Bool(b) => Ok(b.to_string()),
                    Value::Number(n) => Ok(n.to_string()),
                    Value::String(s) => Ok(s.as_str().to_string()),
                    Value::Array(_) | Value::Object(_) => Err(Error {
                        message: "Expressions can't evaluate to arrays or objects.".to_string(),
                        location: self.location(),
                    }),
                }
            }
        }
    }
}

/// A constant string literal—the piece of a [`StringTemplate`] _not_ in `{ }`
#[derive(Clone, Debug)]
pub(crate) struct Constant {
    pub(crate) value: String, // TODO: store string slices instead for improved performance?
    pub(crate) location: Range<usize>,
}

/// A dynamic piece of a [`StringTemplate`], which came from inside `{ }` originally.
#[derive(Clone, Debug)]
pub(crate) struct Expression {
    pub(crate) expression: JSONSelection,
    pub(crate) location: Range<usize>,
}

#[cfg(test)]
mod test_parse {
    use insta::assert_debug_snapshot;

    use super::*;

    #[test]
    fn simple_constant() {
        let template = StringTemplate::from_str("text").expect("simple template should be valid");
        assert_debug_snapshot!(template);
    }

    #[test]
    fn simple_expression() {
        assert_debug_snapshot!(StringTemplate::from_str("{$config.one}").unwrap());
    }
    #[test]
    fn mixed_constant_and_expression() {
        assert_debug_snapshot!(StringTemplate::from_str("text{$config.one}text").unwrap());
    }

    #[test]
    fn expressions_with_nested_braces() {
        assert_debug_snapshot!(
            StringTemplate::from_str("const{$config.one { two { three } }}another-const").unwrap()
        );
    }

    #[test]
    fn missing_closing_braces() {
        assert_debug_snapshot!(
            StringTemplate::from_str("{$config.one"),
            @r###"
        Err(
            Error {
                message: "Invalid expression, missing closing }",
                location: 0..12,
            },
        )
        "###
        )
    }
}

#[cfg(test)]
mod test_interpolate {
    use insta::assert_debug_snapshot;
    use pretty_assertions::assert_eq;
    use serde_json_bytes::json;

    use super::*;
    #[test]
    fn test_interpolate() {
        let template = StringTemplate::from_str("before {$config.one} after").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": "foo"}));
        assert_eq!(template.interpolate(&vars).unwrap(), "before foo after");
    }

    #[test]
    fn test_interpolate_missing_value() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let vars = IndexMap::default();
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_array() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": ["one", "two"]}));
        assert_debug_snapshot!(
            template.interpolate(&vars),
            @r###"
        Err(
            Error {
                message: "Expressions can't evaluate to arrays or objects.",
                location: 1..12,
            },
        )
        "###
        );
    }

    #[test]
    fn test_interpolate_value_bool() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": true}));
        assert_eq!(template.interpolate(&vars).unwrap(), "true");
    }

    #[test]
    fn test_interpolate_value_null() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": null}));
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_number() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": 1}));
        assert_eq!(template.interpolate(&vars).unwrap(), "1");
    }

    #[test]
    fn test_interpolate_value_object() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": {}}));
        assert_debug_snapshot!(
            template.interpolate(&vars),
            @r###"
        Err(
            Error {
                message: "Expressions can't evaluate to arrays or objects.",
                location: 1..12,
            },
        )
        "###
        );
    }

    #[test]
    fn test_interpolate_value_string() {
        let template = StringTemplate::from_str("{$config.one}").unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": "string"}));
        assert_eq!(template.interpolate(&vars).unwrap(), "string");
    }
}

#[cfg(test)]
mod test_get_expressions {
    use super::*;

    #[test]
    fn test_variable_references() {
        let value =
            StringTemplate::from_str("a {$this.a.b.c} b {$args.a.b.c} c {$config.a.b.c}").unwrap();
        let references: Vec<_> = value
            .expressions()
            .map(|e| e.expression.to_string())
            .collect();
        assert_eq!(
            references,
            vec!["$this.a.b.c", "$args.a.b.c", "$config.a.b.c"]
        );
    }
}
