//! A [`StringTemplate`] is a string containing one or more [`Expression`]s.
//! These are used in connector URIs and headers.
//!
//! Parsing (this module) is done by both the router at startup and composition. Validation
//! (in [`crate::sources::connect::validation`]) is done only by composition.

use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde_json_bytes::Value;

use crate::sources::connect::JSONSelection;

/// A parsed string template, containing a series of [`Part`]s.
///
/// The `Const` generic allows consumers to validate constant pieces of the string with a type of
/// their choice. This is specifically just [`http::HeaderValue`] for headers right now.
#[derive(Clone, Debug)]
pub struct StringTemplate<Const = String> {
    pub(crate) parts: Vec<Part<Const>>,
}
impl<Const: FromStr> StringTemplate<Const> {
    /// Parse a [`StringTemplate`]. If this template is nested within another string, provide an
    /// `offset` to correct the locations.
    ///
    /// TODO: Remove the `offset` param once `URLTemplate` can leverage this more directly.
    pub(crate) fn parse(input: &str, mut offset: usize) -> Result<Self, Error> {
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
                let constant = chars
                    .by_ref()
                    .peeking_take_while(|c| *c != '{')
                    .collect::<String>();
                let value = Const::from_str(&constant).map_err(|_unhelpful_err| Error {
                    message: format!("invalid value `{constant}`"),
                    location: offset..offset + constant.len(),
                })?;
                parts.push(Part::Constant(Constant {
                    value,
                    location: offset..offset + constant.len(),
                }));
                offset += constant.len();
            }
        }
        Ok(Self { parts })
    }

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

impl StringTemplate<String> {
    /// Interpolation for when the constant type is a string. This can't be implemented for
    /// arbitrary generic types, so non-string consumers (headers) implement this themselves with
    /// any additional validations/transformations they need.
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<String, Error> {
        self.parts
            .iter()
            .map(|part| part.interpolate(vars))
            .collect()
    }
}

/// Expressions should be written the same as they were originally, even though we don't keep the
/// original source around. So constants are written as-is and expressions are surrounded with `{ }`.
impl<Const: Display> Display for StringTemplate<Const> {
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
pub(crate) enum Part<Const> {
    /// A constant string literal—the piece of a [`StringTemplate`] _not_ in `{ }`
    Constant(Constant<Const>),
    /// A dynamic piece of a [`StringTemplate`], which came from inside `{ }` originally.
    Expression(Expression),
}

impl<T> Part<T> {
    /// Get the original location of the part from the string which was parsed to form the
    /// [`StringTemplate`].
    fn location(&self) -> Range<usize> {
        match self {
            Self::Constant(c) => c.location.clone(),
            Self::Expression(e) => e.location.clone(),
        }
    }
}

/// These generics are a bit of a mess, but what they're saying is given a generic `Const` type,
/// which again is `String` for the main use case but specialized occasionally (like [`http::HeaderValue`] for headers),
/// we can interpolate the value of the part into that type.
///
/// For [`Constant`]s this is easy, just clone the value (thus `Const: Clone`).
///
/// For [`Expression`]s, we first need to interpolate the expression as normal (with [`ApplyTo`]),
/// and then convert the resulting [`Value`] into the `Const` type. For that we require both
/// `Const: FromStr` and `Const: TryFrom<String>` so we don't have to clone all `&str` into `String`s,
/// nor borrow `String` just for them to be re-allocated. The `FromStrErr` and `TryFromStringError`
/// are then required to capture the error types of those two conversion methods.
///
/// So for `Const = String` these are actually all no-ops with infallible conversions, but we allow
/// for [`http::HeaderValue`] to fail.
impl<Const, FromStrErr, TryFromStringError> Part<Const>
where
    Const: Clone,
    Const: FromStr<Err = FromStrErr>,
    FromStrErr: std::error::Error,
    Const: TryFrom<String, Error = TryFromStringError>,
    TryFromStringError: std::error::Error,
{
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<Const, Error> {
        match self {
            Part::Constant(Constant { value, .. }) => Ok(value.clone()),
            Part::Expression(Expression { expression, .. }) => {
                // TODO: do something with the ApplyTo errors
                let (value, _errs) = expression.apply_with_vars(&Value::Null, vars);

                match value.unwrap_or(Value::Null) {
                    Value::Null => Const::from_str("").map_err(|err| Error {
                        message: err.to_string(),
                        location: self.location(),
                    }),
                    Value::Bool(b) => Const::try_from(b.to_string()).map_err(|err| Error {
                        message: err.to_string(),
                        location: self.location(),
                    }),
                    Value::Number(n) => Const::try_from(n.to_string()).map_err(|err| Error {
                        message: err.to_string(),
                        location: self.location(),
                    }),
                    Value::String(s) => Const::from_str(s.as_str()).map_err(|err| Error {
                        message: err.to_string(),
                        location: self.location(),
                    }),
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
pub(crate) struct Constant<T> {
    value: T,
    location: Range<usize>,
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
        let template =
            StringTemplate::<String>::parse("text", 0).expect("simple template should be valid");
        assert_debug_snapshot!(template);
    }

    #[test]
    fn simple_expression() {
        assert_debug_snapshot!(StringTemplate::<String>::parse("{$config.one}", 0).unwrap());
    }
    #[test]
    fn mixed_constant_and_expression() {
        assert_debug_snapshot!(
            StringTemplate::<String>::parse("text{$config.one}text", 0).unwrap()
        );
    }

    #[test]
    fn offset() {
        assert_debug_snapshot!(
            StringTemplate::<String>::parse("text{$config.one}text", 9).unwrap()
        );
    }

    #[test]
    fn expressions_with_nested_braces() {
        assert_debug_snapshot!(
            StringTemplate::<String>::parse("const{$config.one { two { three } }}another-const", 0)
                .unwrap()
        );
    }

    #[test]
    fn missing_closing_braces() {
        assert_debug_snapshot!(
            StringTemplate::<String>::parse("{$config.one", 0),
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
        let template = StringTemplate::<String>::parse("before {$config.one} after", 0).unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": "foo"}));
        assert_eq!(template.interpolate(&vars).unwrap(), "before foo after");
    }

    #[test]
    fn test_interpolate_missing_value() {
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
        let vars = IndexMap::default();
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_array() {
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
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
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": true}));
        assert_eq!(template.interpolate(&vars).unwrap(), "true");
    }

    #[test]
    fn test_interpolate_value_null() {
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": null}));
        assert_eq!(template.interpolate(&vars).unwrap(), "");
    }

    #[test]
    fn test_interpolate_value_number() {
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
        let mut vars = IndexMap::default();
        vars.insert("$config".to_string(), json!({"one": 1}));
        assert_eq!(template.interpolate(&vars).unwrap(), "1");
    }

    #[test]
    fn test_interpolate_value_object() {
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
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
        let template = StringTemplate::<String>::parse("{$config.one}", 0).unwrap();
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
            StringTemplate::<String>::parse("a {$this.a.b.c} b {$args.a.b.c} c {$config.a.b.c}", 0)
                .unwrap();
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
