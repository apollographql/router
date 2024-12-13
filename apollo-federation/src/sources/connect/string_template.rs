//! A [`StringTemplate`] is a string containing one or more [`Expression`]s.
//! These are used in connector URIs and headers.

use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde_json_bytes::Value;

use crate::sources::connect::JSONSelection;

/// A parsed string template, containing a series of [`Part`]s.
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
                        location: start_of_parse_error..(start_of_parse_error + expression.len()),
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
    pub(crate) fn interpolate(&self, vars: &IndexMap<String, Value>) -> Result<String, Error> {
        self.parts
            .iter()
            .map(|part| part.interpolate(vars))
            .collect()
    }
}

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

#[derive(Debug, PartialEq)]
pub struct Error {
    pub message: String,
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
    fn location(&self) -> Range<usize> {
        match self {
            Self::Constant(c) => c.location.clone(),
            Self::Expression(e) => e.location.clone(),
        }
    }
}

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
                        message: "Header expressions can't evaluate to arrays or objects."
                            .to_string(),
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
