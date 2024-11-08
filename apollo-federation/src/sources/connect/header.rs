//! Headers defined in connectors `@source` and `@connect` directives.

use std::str::FromStr;

use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::none_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::recognize;
use nom::error::ErrorKind;
use nom::error::ParseError;
use nom::multi::many1;
use nom::sequence::delimited;
use nom::IResult;
use nom_locate::LocatedSpan;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;

use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariableParseError;
use crate::sources::connect::variable::VariableReference;

/// A header value, optionally containing variable references.
#[derive(Debug, PartialEq, Clone)]
pub struct HeaderValue {
    parts: Vec<HeaderValuePart>,
}

impl HeaderValue {
    fn new(parts: Vec<HeaderValuePart>) -> Self {
        Self { parts }
    }

    fn parse(input: Span) -> IResult<Span, Self> {
        all_consuming(map(many1(HeaderValuePart::parse), Self::new))(input)
    }

    pub(crate) fn variable_references(
        &self,
    ) -> impl Iterator<Item = &VariableReference<Namespace>> {
        self.parts.iter().filter_map(|part| {
            if let HeaderValuePart::Variable(var) = part {
                Some(var)
            } else {
                None
            }
        })
    }

    /// Replace variable references in the header value with the given variable definitions.
    ///
    /// # Errors
    /// Returns an error if a variable used in the header value is not defined.
    pub fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Result<String, String> {
        let mut result = String::new();
        for part in &self.parts {
            match part {
                HeaderValuePart::Text(text) => result.push_str(text),
                HeaderValuePart::Variable(var) => {
                    let var_path_bytes = ByteString::from(var.to_string());
                    let value = vars
                        .get(&var_path_bytes)
                        .ok_or_else(|| format!("Missing variable: {var}"))?;
                    let value = if let JSON::String(string) = value {
                        string.as_str().to_string()
                    } else {
                        value.to_string()
                    };
                    result.push_str(value.as_str());
                }
            }
        }
        Ok(result)
    }
}

impl FromStr for HeaderValue {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match Self::parse(Span::new(s)) {
            Ok((_, value)) => Ok(value),
            Err(e) => Err(format!("Invalid header value: {}", e)),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
enum HeaderValuePart {
    Text(String),
    Variable(VariableReference<Namespace>),
}

impl HeaderValuePart {
    fn parse(input: Span) -> IResult<Span, Self> {
        alt((
            map(variable_reference, Self::Variable),
            map(map(text, |s| s.fragment().to_string()), Self::Text),
        ))(input)
    }
}

type Span<'a> = LocatedSpan<&'a str>;

fn text(input: Span) -> IResult<Span, Span> {
    recognize(many1(none_of("{")))(input)
}

fn variable_reference(input: Span) -> IResult<Span, VariableReference<Namespace>> {
    delimited(
        char('{'),
        |input| {
            super::variable::variable_reference(input).map_err(|e| match e {
                nom::Err::Error(VariableParseError::Nom(span, e)) => {
                    nom::Err::Error(nom::error::Error::from_error_kind(span, e))
                }
                _ => nom::Err::Error(nom::error::Error::from_error_kind(input, ErrorKind::IsNot)),
            })
        },
        char('}'),
    )(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::connect::variable::VariableNamespace;
    use crate::sources::connect::variable::VariablePathPart;

    #[test]
    fn test_text() {
        let remove_spans =
            |(a, b): (Span, Span)| (a.fragment().to_string(), b.fragment().to_string());
        assert_eq!(
            text(Span::new("text")).map(remove_spans),
            Ok(("".into(), "text".into()))
        );
        assert!(text(Span::new("{$config.one}")).is_err());
        assert_eq!(
            text(Span::new("text{$config.one}")).map(remove_spans),
            Ok(("{$config.one}".into(), "text".into()))
        );
    }

    #[test]
    fn test_header_value_part_parse() {
        assert_eq!(
            HeaderValuePart::parse(Span::new("text")).map(|(a, b)| (*a.fragment(), b)),
            Ok(("", HeaderValuePart::Text("text".to_string())))
        );
        assert_eq!(
            HeaderValuePart::parse(Span::new("{$config.one}")).map(|(a, b)| (*a.fragment(), b)),
            Ok((
                "",
                HeaderValuePart::Variable(VariableReference {
                    namespace: VariableNamespace {
                        namespace: Namespace::Config,
                        location: 1..8,
                    },
                    path: vec![VariablePathPart {
                        part: "one".to_string(),
                        location: 9..12
                    }],
                    location: 1..12
                })
            ))
        );
        assert_eq!(
            HeaderValuePart::parse(Span::new("text{$config.one}")).map(|(a, b)| (*a.fragment(), b)),
            Ok(("{$config.one}", HeaderValuePart::Text("text".to_string())))
        );
    }

    #[test]
    fn test_header_value_parse() {
        assert_eq!(
            "text".parse(),
            Ok(HeaderValue {
                parts: vec![HeaderValuePart::Text("text".to_string())]
            })
        );
        assert_eq!(
            "{$config.one}".parse(),
            Ok(HeaderValue {
                parts: vec![HeaderValuePart::Variable(VariableReference {
                    namespace: VariableNamespace {
                        namespace: Namespace::Config,
                        location: 1..8,
                    },
                    path: vec![VariablePathPart {
                        part: "one".to_string(),
                        location: 9..12
                    }],
                    location: 1..12
                })]
            })
        );
        assert_eq!(
            "text{$config.one}text".parse(),
            Ok(HeaderValue {
                parts: vec![
                    HeaderValuePart::Text("text".to_string()),
                    HeaderValuePart::Variable(VariableReference {
                        namespace: VariableNamespace {
                            namespace: Namespace::Config,
                            location: 5..12,
                        },
                        path: vec![VariablePathPart {
                            part: "one".to_string(),
                            location: 13..16
                        }],
                        location: 5..16
                    }),
                    HeaderValuePart::Text("text".to_string())
                ]
            })
        );
        assert_eq!(
            "    {$config.one}    ".parse(),
            Ok(HeaderValue {
                parts: vec![
                    HeaderValuePart::Text("    ".to_string()),
                    HeaderValuePart::Variable(VariableReference {
                        namespace: VariableNamespace {
                            namespace: Namespace::Config,
                            location: 5..12,
                        },
                        path: vec![VariablePathPart {
                            part: "one".to_string(),
                            location: 13..16
                        }],
                        location: 5..16
                    }),
                    HeaderValuePart::Text("    ".to_string())
                ]
            })
        );
    }

    #[test]
    fn test_interpolate() {
        let value = HeaderValue::from_str("before {$config.one} after").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::String("foo".into()));
        assert_eq!(value.interpolate(&vars), Ok("before foo after".into()));
    }

    #[test]
    fn test_interpolate_missing_value() {
        let value = HeaderValue::from_str("{$config.one}").unwrap();
        let vars = Map::new();
        assert_eq!(
            value.interpolate(&vars),
            Err("Missing variable: $config.one".to_string())
        );
    }

    #[test]
    fn test_interpolate_value_array() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::Array(vec!["one".into(), "two".into()]));
        assert_eq!(
            Ok("[\"one\",\"two\"]".into()),
            header_value.interpolate(&vars)
        );
    }

    #[test]
    fn test_interpolate_value_bool() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::Bool(true));
        assert_eq!(Ok("true".into()), header_value.interpolate(&vars));
    }

    #[test]
    fn test_interpolate_value_null() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::Null);
        assert_eq!(Ok("null".into()), header_value.interpolate(&vars));
    }

    #[test]
    fn test_interpolate_value_number() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::Number(1.into()));
        assert_eq!(Ok("1".into()), header_value.interpolate(&vars));
    }

    #[test]
    fn test_interpolate_value_object() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::Object(Map::new()));
        assert_eq!(Ok("{}".into()), header_value.interpolate(&vars));
    }

    #[test]
    fn test_interpolate_value_string() {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", JSON::String("string".into()));
        assert_eq!(Ok("string".into()), header_value.interpolate(&vars));
    }

    #[test]
    fn test_variable_references() {
        let value =
            HeaderValue::from_str("a {$this.a.b.c} b {$args.a.b.c} c {$config.a.b.c}").unwrap();
        let references: Vec<_> = value
            .variable_references()
            .map(|variable| variable.to_string())
            .collect();
        assert_eq!(
            references,
            vec!["$this.a.b.c", "$args.a.b.c", "$config.a.b.c"]
        );
    }

    #[test]
    fn test_variable_references_with_error() {
        assert!(HeaderValue::from_str("a {$this} b {$unknown} c {$config}").is_err());
    }
}
