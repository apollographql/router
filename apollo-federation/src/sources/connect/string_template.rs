//! A [`StringTemplate`] is a string containing one or more [`Expression`]s.
//! These are used in connector URIs and headers.

use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use serde_json_bytes::Value;
use shape::Shape;
use shape::ShapeCase;

use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;

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

impl Expression {
    pub(crate) fn validate(
        &self,
        shape_lookup: &IndexMap<&str, Shape>,
        var_lookup: &IndexMap<Namespace, Shape>,
    ) -> Result<(), Vec<Error>> {
        let shape = self.expression.shape();
        let errors: Vec<Error> = shape
            .errors()
            .map(|err| Error {
                message: err.message.clone(),
                location: err
                    .range
                    .as_ref()
                    .map(|range| range.start + self.location.start..range.end + self.location.start)
                    .unwrap_or(self.location.clone()),
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }

        Self::validate_shape(&shape, shape_lookup, var_lookup).map_err(|message| {
            vec![Error {
                message,
                location: self.location.clone(),
            }]
        })
    }

    /// Validate that the shape is an acceptable output shape for an Expression.
    ///
    /// TODO: Some day, whether objects or arrays are allowed will be dependent on &self (i.e., is the * modifier used)
    fn validate_shape(
        shape: &Shape,
        shape_lookup: &IndexMap<&str, Shape>,
        var_lookup: &IndexMap<Namespace, Shape>,
    ) -> Result<(), String> {
        match shape.case() {
            ShapeCase::Array { .. } => Err("URIs can't contain arrays".to_string()),
            ShapeCase::Object { .. } => Err("URIs can't contain objects".to_string()),
            ShapeCase::One(shapes) | ShapeCase::All(shapes) => {
                for shape in shapes {
                    Self::validate_shape(shape, shape_lookup, var_lookup)?;
                }
                Ok(())
            }
            ShapeCase::Name(name, key) => {
                let mut shape = if name == "$root" {
                    return Err(format!(
                        "`{key}` must start with an argument name, like `$this` or `$args`",
                        key = key.iter().map(|key| key.to_string()).join(".")
                    ));
                } else if name.starts_with('$') {
                    let namespace = Namespace::from_str(name).map_err(|_| {
                        format!(
                            "unknown variable `{name}`, must be one of {namespaces}",
                            namespaces = var_lookup.keys().map(|ns| ns.as_str()).join(", ")
                        )
                    })?;
                    var_lookup
                        .get(&namespace)
                        .ok_or_else(|| {
                            format!(
                                "{namespace} is not allowed here, must be one of {namespaces}",
                                namespaces = var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                            )
                        })?
                        .clone()
                } else {
                    shape_lookup
                        .get(name.as_str())
                        .cloned()
                        .ok_or_else(|| format!("unknown type `{name}`"))?
                };
                let mut path = name.clone();
                for key in key {
                    let child = shape.child(key);
                    if child.is_none() {
                        return Err(format!("`{path}` doesn't have a field named `{key}`"));
                    }
                    shape = child;
                    path = format!("{path}.{key}");
                }
                Self::validate_shape(&shape, shape_lookup, var_lookup)
            }
            ShapeCase::Error(shape::Error { message, .. }) => Err(message.clone()),
            ShapeCase::None
            | ShapeCase::Bool(_)
            | ShapeCase::String(_)
            | ShapeCase::Int(_)
            | ShapeCase::Float
            | ShapeCase::Null => Ok(()), // We use null as any/unknown right now, so don't say anything about it
        }
    }
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
        assert_debug_snapshot!(StringTemplate::<String>::parse("text{$config.one}text", 0).unwrap());
    }

    #[test]
    fn offset() {
        assert_debug_snapshot!(StringTemplate::<String>::parse("text{$config.one}text", 9).unwrap());
    }

    #[test]
    fn expressions_with_nested_braces() {
        assert_debug_snapshot!(StringTemplate::<String>::parse(
            "const{$config.one { two { three } }}another-const",
            0
        )
        .unwrap());
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
