// A LitExpr (short for LiteralExpression) is similar to a JSON value (or
// serde_json::Value), with the addition of PathSelection as a possible leaf
// value, so literal expressions passed to -> methods (via MethodArgs) can
// capture both field and argument values and sub-paths, in addition to constant
// JSON structures and values.

use apollo_compiler::collections::IndexMap;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::multi::many1;
use nom::sequence::delimited;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde::Serialize;

use super::helpers::spaces_or_comments;
use super::parser::parse_string_literal;
use super::parser::Key;
use super::parser::PathSelection;

#[derive(Debug, PartialEq, Clone)]
pub enum LitExpr {
    String(String),
    Number(serde_json::Number),
    Bool(bool),
    Null,
    Object(IndexMap<String, LitExpr>),
    Array(Vec<LitExpr>),
    Path(PathSelection),
}

impl LitExpr {
    // LitExpr      ::= LitPrimitive | LitObject | LitArray | PathSelection
    // LitPrimitive ::= LitString | LitNumber | "true" | "false" | "null"
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
                map(PathSelection::parse, Self::Path),
            )),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, value, _))| (input, value))
    }

    // LitNumber ::= "-"? ([0-9]+ ("." [0-9]*)? | "." [0-9]+)
    fn parse_number(input: &str) -> IResult<&str, Self> {
        let (suffix, (neg, _, num)) = delimited(
            spaces_or_comments,
            tuple((
                opt(char('-')),
                spaces_or_comments,
                alt((
                    recognize(pair(
                        many1(one_of("0123456789")),
                        opt(preceded(char('.'), many0(one_of("0123456789")))),
                    )),
                    recognize(pair(tag("."), many1(one_of("0123456789")))),
                )),
            )),
            spaces_or_comments,
        )(input)?;

        let mut number = String::new();
        if let Some('-') = neg {
            number.push('-');
        }
        number.push_str(num);

        if let Ok(lit_number) = number.parse().map(Self::Number) {
            Ok((suffix, lit_number))
        } else {
            Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::IsNot,
            )))
        }
    }

    // LitObject ::= "{" (LitProperty ("," LitProperty)* ","?)? "}"
    fn parse_object(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('{'), spaces_or_comments)),
            map(
                opt(tuple((
                    Self::parse_property,
                    many0(preceded(char(','), Self::parse_property)),
                    opt(char(',')),
                ))),
                |properties| {
                    let mut output = IndexMap::default();
                    if let Some(((first_key, first_value), rest, _trailing_comma)) = properties {
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

    // LitProperty ::= Key ":" LitExpr
    fn parse_property(input: &str) -> IResult<&str, (String, Self)> {
        tuple((Key::parse, char(':'), Self::parse))(input)
            .map(|(input, (key, _, value))| (input, (key.as_string(), value)))
    }

    // LitArray ::= "[" (LitExpr ("," LitExpr)* ","?)? "]"
    fn parse_array(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('['), spaces_or_comments)),
            map(
                opt(tuple((
                    Self::parse,
                    many0(preceded(char(','), Self::parse)),
                    opt(char(',')),
                ))),
                |elements| {
                    let mut output = vec![];
                    if let Some((first, rest, _trailing_comma)) = elements {
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
            Self::Number(n) => n.as_i64(),
            _ => None,
        }
    }
}

impl Serialize for LitExpr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        match self {
            Self::String(s) => serializer.serialize_str(s),
            Self::Number(n) => n.serialize(serializer),
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
