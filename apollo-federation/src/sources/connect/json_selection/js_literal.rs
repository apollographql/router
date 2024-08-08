// A JSLiteral is similar to a JSON value (or serde_json::Value), with the
// addition of PathSelection as a possible leaf value, so literal expressions
// passed to -> methods (via MethodArgs) can capture both field and argument
// values and sub-paths, in addition to constant JSON structures and values.

use apollo_compiler::collections::IndexMap;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::sequence::delimited;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde::Serialize;
use serde_json_bytes::Value as JSON;

use super::helpers::spaces_or_comments;
use super::parser::parse_string_literal;
use super::parser::Key;
use super::parser::PathSelection;

#[derive(Debug, PartialEq, Clone)]
pub enum JSLiteral {
    String(String),
    Number(String),
    Bool(bool),
    Null,
    Object(IndexMap<String, JSLiteral>),
    Array(Vec<JSLiteral>),
    Path(PathSelection),
}

impl JSLiteral {
    // JSLiteral   ::= JSPrimitive | JSObject | JSArray | PathSelection
    // JSPrimitive ::= StringLiteral | JSNumber | "true" | "false" | "null"
    pub fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            alt((
                map(parse_string_literal, Self::String),
                Self::parse_number,
                map(tag("true"), |_| Self::Bool(true)),
                map(tag("false"), |_| Self::Bool(false)),
                map(tag("null"), |_| Self::Null),
                Self::parse_object,
                Self::parse_array,
                // TODO Disallow KeyPath PathSelection in JSLiteral?
                map(PathSelection::parse, Self::Path),
            )),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, value, _))| (input, value))
    }

    // JSNumber    ::= "-"? (UnsignedInt ("." [0-9]*)? | "." [0-9]+)
    // UnsignedInt ::= "0" | [1-9] NO_SPACE [0-9]*
    fn parse_number(input: &str) -> IResult<&str, Self> {
        delimited(
            spaces_or_comments,
            tuple((
                opt(char('-')),
                spaces_or_comments,
                tuple((
                    recognize(alt((
                        recognize(char('0')),
                        recognize(pair(one_of("123456789"), many0(one_of("0123456789")))),
                    ))),
                    opt(preceded(char('.'), recognize(many0(one_of("0123456789"))))),
                )),
            )),
            spaces_or_comments,
        )(input)
        .map(|(input, (neg, _, (integer, decimal)))| {
            let mut number = String::new();
            if let Some(neg) = neg {
                number.push(neg);
            }
            number.push_str(integer);
            if let Some(decimal) = decimal {
                number.push('.');
                number.push_str(decimal);
            }
            (input, Self::Number(number))
        })
    }

    // JSObject ::= "{" (JSProperty ("," JSProperty)*)? "}"
    fn parse_object(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('{'), spaces_or_comments)),
            map(
                opt(tuple((
                    Self::parse_property,
                    many0(preceded(char(','), Self::parse_property)),
                ))),
                |properties| {
                    let mut output = IndexMap::default();
                    if let Some(((first_key, first_value), rest)) = properties {
                        output.insert(first_key, first_value);
                        for (key, value) in rest {
                            output.insert(key, value);
                        }
                    }
                    Self::Object(output)
                },
            ),
            tuple((spaces_or_comments, char('}'), spaces_or_comments)),
        )(input)
    }

    // JSProperty ::= Key ":" JSONLiteral
    fn parse_property(input: &str) -> IResult<&str, (String, Self)> {
        tuple((Key::parse, char(':'), Self::parse))(input)
            .map(|(input, (key, _, value))| (input, (key.to_string(), value)))
    }

    // JSArray ::= "[" (JSONLiteral ("," JSONLiteral)*)? "]"
    fn parse_array(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('['), spaces_or_comments)),
            map(
                opt(tuple((
                    Self::parse,
                    many0(preceded(char(','), Self::parse)),
                ))),
                |elements| {
                    let mut output = vec![];
                    if let Some((first, rest)) = elements {
                        output.push(first);
                        output.extend(rest);
                    }
                    Self::Array(output)
                },
            ),
            tuple((spaces_or_comments, char(']'), spaces_or_comments)),
        )(input)
    }

    pub(super) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(n) => match serde_json_bytes::serde_json::from_str(n.as_str()) {
                Ok(JSON::Number(n)) => n.as_i64(),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Serialize for JSLiteral {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        match self {
            Self::String(s) => serializer.serialize_str(s),
            Self::Number(n) => serializer.serialize_str(n),
            Self::Bool(b) => serializer.serialize_bool(*b),
            Self::Null => serializer.serialize_none(),
            Self::Object(map) => {
                let mut state = serializer.serialize_map(Some(map.len()))?;
                for (key, value) in map {
                    state.serialize_entry(key, value)?;
                }
                state.end()
            }
            Self::Array(vec) => {
                let mut state = serializer.serialize_seq(Some(vec.len()))?;
                for value in vec {
                    state.serialize_element(value)?;
                }
                state.end()
            }
            Self::Path(path) => path.serialize(serializer),
        }
    }
}
