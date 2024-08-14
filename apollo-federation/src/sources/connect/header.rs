//! Headers defined in connectors `@source` and `@connect` directives.
use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::alpha1;
use nom::character::complete::alphanumeric1;
use nom::character::complete::char;
use nom::character::complete::none_of;
use nom::combinator::map;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::multi::many1;
use nom::sequence::delimited;
use nom::sequence::pair;
use nom::IResult;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;

/// A header value, optionally containing variable references.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct HeaderValue {
    parts: Vec<HeaderValuePart>,
}

impl HeaderValue {
    fn new(parts: Vec<HeaderValuePart>) -> Self {
        Self { parts }
    }

    fn parse(input: &str) -> IResult<&str, Self> {
        map(many1(HeaderValuePart::parse), Self::new)(input)
    }

    /// Replace variable references in the header value with the given variable definitions.
    ///
    /// # Errors
    /// Returns an error if a variable used in the header value is not defined or if a variable
    /// value is not a string.
    pub fn interpolate(&self, vars: &Map<ByteString, JSON>) -> Result<String, String> {
        let mut result = String::new();
        for part in &self.parts {
            match part {
                HeaderValuePart::Text(text) => result.push_str(text),
                HeaderValuePart::Variable(var) => {
                    let var_path_bytes = ByteString::from(var.path.as_str());
                    let value = vars
                        .get(&var_path_bytes)
                        .ok_or_else(|| format!("Missing variable: {}", var.path))?;
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
        match Self::parse(s) {
            Ok((_, value)) => Ok(value),
            Err(e) => Err(format!("Invalid header value: {}", e)),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
enum HeaderValuePart {
    Text(String),
    Variable(VariableReference),
}

impl HeaderValuePart {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            map(VariableReference::parse, Self::Variable),
            map(map(text, String::from), Self::Text),
        ))(input)
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
struct VariableReference {
    path: String,
}

impl VariableReference {
    fn new(path: String) -> Self {
        Self { path }
    }

    fn parse(input: &str) -> IResult<&str, Self> {
        map(map(variable_reference, String::from), Self::new)(input)
    }
}

fn text(input: &str) -> IResult<&str, &str> {
    recognize(many1(none_of("{")))(input)
}

fn identifier(input: &str) -> IResult<&str, &str> {
    recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ))(input)
}

fn namespace(input: &str) -> IResult<&str, &str> {
    recognize(tag("$config"))(input)
}

fn path(input: &str) -> IResult<&str, &str> {
    recognize(pair(namespace, many1(pair(char('.'), identifier))))(input)
}

fn variable_reference(input: &str) -> IResult<&str, &str> {
    delimited(char('{'), path, char('}'))(input)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn test_identifier() {
        assert_eq!(identifier("_"), Ok(("", "_")));
        assert_eq!(identifier("a"), Ok(("", "a")));
        assert_eq!(identifier("test"), Ok(("", "test")));
        assert_eq!(identifier("test123"), Ok(("", "test123")));
        assert_eq!(identifier("_test"), Ok(("", "_test")));
        assert_eq!(identifier("test_123"), Ok(("", "test_123")));
        assert_eq!(identifier("test_123 more"), Ok((" more", "test_123")));
    }

    #[test]
    fn test_namespace() {
        assert_eq!(namespace("$config"), Ok(("", "$config")));
        assert_eq!(namespace("$config.one"), Ok((".one", "$config")));
        assert_eq!(namespace("$config.one.two"), Ok((".one.two", "$config")));
        assert_eq!(namespace("$config}more"), Ok(("}more", "$config")));
    }

    #[test]
    fn test_path() {
        assert_eq!(path("$config.one"), Ok(("", "$config.one")));
        assert_eq!(path("$config.one.two"), Ok(("", "$config.one.two")));
        assert_eq!(path("$config._one._two"), Ok(("", "$config._one._two")));
        assert_eq!(
            path("$config.one.two}more"),
            Ok(("}more", "$config.one.two"))
        );
    }

    #[test]
    fn test_variable_reference() {
        assert!(variable_reference("{$config}").is_err());
        assert!(variable_reference("{$not_a_namespace.one}").is_err());
        assert_eq!(variable_reference("{$config.one}"), Ok(("", "$config.one")));
        assert_eq!(
            variable_reference("{$config.one.two}"),
            Ok(("", "$config.one.two"))
        );
        assert_eq!(
            variable_reference("{$config.one}more"),
            Ok(("more", "$config.one"))
        );
    }

    #[test]
    fn test_variable_reference_parse() {
        assert_eq!(
            VariableReference::parse("{$config.one}"),
            Ok((
                "",
                VariableReference {
                    path: "$config.one".to_string()
                }
            ))
        );
        assert_eq!(
            VariableReference::parse("{$config.one.two}"),
            Ok((
                "",
                VariableReference {
                    path: "$config.one.two".to_string()
                }
            ))
        );
    }

    #[test]
    fn test_text() {
        assert_eq!(text("text"), Ok(("", "text")));
        assert!(text("{$config.one}").is_err());
        assert_eq!(text("text{$config.one}"), Ok(("{$config.one}", "text")));
    }

    #[test]
    fn test_header_value_part_parse() {
        assert_eq!(
            HeaderValuePart::parse("text"),
            Ok(("", HeaderValuePart::Text("text".to_string())))
        );
        assert_eq!(
            HeaderValuePart::parse("{$config.one}"),
            Ok((
                "",
                HeaderValuePart::Variable(VariableReference {
                    path: "$config.one".to_string()
                })
            ))
        );
        assert_eq!(
            HeaderValuePart::parse("text{$config.one}"),
            Ok(("{$config.one}", HeaderValuePart::Text("text".to_string())))
        );
    }

    #[test]
    fn test_header_value_parse() {
        assert_eq!(
            HeaderValue::parse("text"),
            Ok((
                "",
                HeaderValue {
                    parts: vec![HeaderValuePart::Text("text".to_string())]
                }
            ))
        );
        assert_eq!(
            HeaderValue::parse("{$config.one}"),
            Ok((
                "",
                HeaderValue {
                    parts: vec![HeaderValuePart::Variable(VariableReference {
                        path: "$config.one".to_string()
                    })]
                }
            ))
        );
        assert_eq!(
            HeaderValue::parse("text{$config.one}text"),
            Ok((
                "",
                HeaderValue {
                    parts: vec![
                        HeaderValuePart::Text("text".to_string()),
                        HeaderValuePart::Variable(VariableReference {
                            path: "$config.one".to_string()
                        }),
                        HeaderValuePart::Text("text".to_string())
                    ]
                }
            ))
        );
        assert_eq!(
            HeaderValue::parse("    {$config.one}    "),
            Ok((
                "",
                HeaderValue {
                    parts: vec![
                        HeaderValuePart::Text("    ".to_string()),
                        HeaderValuePart::Variable(VariableReference {
                            path: "$config.one".to_string()
                        }),
                        HeaderValuePart::Text("    ".to_string())
                    ]
                }
            ))
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

    #[rstest]
    #[case(JSON::Array(vec!["one".into(), "two".into()]), Ok("[\"one\",\"two\"]".into()))]
    #[case(JSON::Bool(true), Ok("true".into()))]
    #[case(JSON::Null, Ok("null".into()))]
    #[case(JSON::Number(1.into()), Ok("1".into()))]
    #[case(JSON::Object(Map::new()), Ok("{}".into()))]
    #[case(JSON::String("string".into()), Ok("string".into()))]
    fn test_interpolate_value_not_a_string(
        #[case] value: JSON,
        #[case] expected: Result<String, String>,
    ) {
        let header_value = HeaderValue::from_str("{$config.one}").unwrap();
        let mut vars = Map::new();
        vars.insert("$config.one", value);
        assert_eq!(expected, header_value.interpolate(&vars));
    }
}
