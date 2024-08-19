// A LitExpr (short for LiteralExpression) is similar to a JSON value (or
// serde_json::Value), with the addition of PathSelection as a possible leaf
// value, so literal expressions passed to -> methods (via MethodArgs) can
// capture both field and argument values and sub-paths, in addition to constant
// JSON structures and values.

use std::hash::Hash;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
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

use super::helpers::spaces_or_comments;
use super::parser::parse_string_literal;
use super::parser::Key;
use super::parser::PathSelection;
use super::CollectVarPaths;

#[derive(Debug, PartialEq, Eq, Clone)]
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
        let (suffix, (neg, _spaces, num)) = delimited(
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
        if num.starts_with('.') {
            // The serde_json::Number::parse method requires a leading digit
            // before the decimal point.
            number.push('0');
        }
        number.push_str(num);
        if num.ends_with('.') {
            // The serde_json::Number::parse method requires a trailing digit
            // after the decimal point.
            number.push('0');
        }

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
                        output.insert(first_key.to_string(), first_value);
                        for (key, value) in rest {
                            output.insert(key.to_string(), value);
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

impl CollectVarPaths for LitExpr {
    fn collect_var_paths(&self) -> IndexSet<&PathSelection> {
        let mut paths = IndexSet::default();
        match self {
            Self::String(_) | Self::Number(_) | Self::Bool(_) | Self::Null => {}
            Self::Object(map) => {
                for value in map.values() {
                    paths.extend(value.collect_var_paths());
                }
            }
            Self::Array(vec) => {
                for value in vec {
                    paths.extend(value.collect_var_paths());
                }
            }
            Self::Path(path) => {
                paths.extend(path.collect_var_paths());
            }
        }
        paths
    }
}

impl Hash for LitExpr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::String(s) => s.hash(state),
            Self::Number(n) => n.to_string().hash(state),
            Self::Bool(b) => b.hash(state),
            Self::Null => "null".hash(state),
            Self::Object(map) => {
                // This hashing strategy makes key ordering significant, which
                // is fine because we don't have an object-order-insensitive
                // equality check for LitExpr. In other words, LitExpr::Object
                // behaves like a list of key-value pairs, preserving the order
                // of the source syntax. Once this LitExpr becomes a JSON value,
                // we can use the order-insensivity of JSON objects.
                map.iter().for_each(|key_value| {
                    key_value.hash(state);
                });
            }
            Self::Array(vec) => vec.hash(state),
            Self::Path(path) => path.hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::connect::json_selection::PathList;

    #[test]
    fn test_lit_expr_parse_primitives() {
        assert_eq!(
            LitExpr::parse("'hello'"),
            Ok(("", LitExpr::String("hello".to_string()))),
        );
        assert_eq!(
            LitExpr::parse("\"hello\""),
            Ok(("", LitExpr::String("hello".to_string()))),
        );

        assert_eq!(
            LitExpr::parse("123"),
            Ok(("", LitExpr::Number(serde_json::Number::from(123)))),
        );
        assert_eq!(
            LitExpr::parse("-123"),
            Ok(("", LitExpr::Number(serde_json::Number::from(-123)))),
        );
        assert_eq!(
            LitExpr::parse(" - 123 "),
            Ok(("", LitExpr::Number(serde_json::Number::from(-123)))),
        );
        assert_eq!(
            LitExpr::parse("123.456"),
            Ok((
                "",
                LitExpr::Number(serde_json::Number::from_f64(123.456).unwrap())
            )),
        );
        assert_eq!(
            LitExpr::parse(".456"),
            Ok((
                "",
                LitExpr::Number(serde_json::Number::from_f64(0.456).unwrap())
            )),
        );
        assert_eq!(
            LitExpr::parse("-.456"),
            Ok((
                "",
                LitExpr::Number(serde_json::Number::from_f64(-0.456).unwrap())
            )),
        );
        assert_eq!(
            LitExpr::parse("123."),
            Ok((
                "",
                LitExpr::Number(serde_json::Number::from_f64(123.0).unwrap())
            )),
        );
        assert_eq!(
            LitExpr::parse("-123."),
            Ok((
                "",
                LitExpr::Number(serde_json::Number::from_f64(-123.0).unwrap())
            )),
        );

        assert_eq!(LitExpr::parse("true"), Ok(("", LitExpr::Bool(true))));
        assert_eq!(LitExpr::parse(" true "), Ok(("", LitExpr::Bool(true))));
        assert_eq!(LitExpr::parse("false"), Ok(("", LitExpr::Bool(false))));
        assert_eq!(LitExpr::parse(" false "), Ok(("", LitExpr::Bool(false))));
        assert_eq!(LitExpr::parse("null"), Ok(("", LitExpr::Null)));
        assert_eq!(LitExpr::parse(" null "), Ok(("", LitExpr::Null)));
    }

    #[test]
    fn test_lit_expr_parse_objects() {
        assert_eq!(
            LitExpr::parse("{'a': 1}"),
            Ok((
                "",
                LitExpr::Object({
                    let mut map = IndexMap::default();
                    map.insert(
                        "a".to_string(),
                        LitExpr::Number(serde_json::Number::from(1)),
                    );
                    map
                })
            ))
        );

        {
            let expected = LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    "a".to_string(),
                    LitExpr::Number(serde_json::Number::from(1)),
                );
                map.insert(
                    "b".to_string(),
                    LitExpr::Number(serde_json::Number::from(2)),
                );
                map
            });
            assert_eq!(
                LitExpr::parse("{'a': 1, 'b': 2}"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("{ a : 1, 'b': 2}"),
                Ok(("", expected.clone()))
            );
            assert_eq!(LitExpr::parse("{ a : 1, b: 2}"), Ok(("", expected.clone())));
            assert_eq!(
                LitExpr::parse("{ \"a\" : 1, \"b\": 2 }"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("{ \"a\" : 1, b: 2 }"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("{ a : 1, \"b\": 2 }"),
                Ok(("", expected.clone()))
            );
        }
    }

    #[test]
    fn test_lit_expr_parse_arrays() {
        assert_eq!(
            LitExpr::parse("[1, 2]"),
            Ok((
                "",
                LitExpr::Array(vec![
                    LitExpr::Number(serde_json::Number::from(1)),
                    LitExpr::Number(serde_json::Number::from(2)),
                ])
            ))
        );

        assert_eq!(
            LitExpr::parse("[1, true, 'three']"),
            Ok((
                "",
                LitExpr::Array(vec![
                    LitExpr::Number(serde_json::Number::from(1)),
                    LitExpr::Bool(true),
                    LitExpr::String("three".to_string()),
                ])
            ))
        );
    }

    #[test]
    fn test_lit_expr_parse_paths() {
        {
            let expected = LitExpr::Path(PathSelection {
                path: PathList::Key(
                    Key::Field("a".to_string()),
                    Box::new(PathList::Key(
                        Key::Field("b".to_string()),
                        Box::new(PathList::Key(
                            Key::Field("c".to_string()),
                            Box::new(PathList::Empty),
                        )),
                    )),
                ),
            });
            assert_eq!(LitExpr::parse("a.b.c"), Ok(("", expected.clone())));
            assert_eq!(LitExpr::parse(" a . b . c "), Ok(("", expected.clone())));
        }

        {
            let expected = LitExpr::Path(PathSelection {
                path: PathList::Key(Key::Field("data".to_string()), Box::new(PathList::Empty)),
            });
            assert_eq!(LitExpr::parse(".data"), Ok(("", expected.clone())));
            assert_eq!(LitExpr::parse(" . data "), Ok(("", expected.clone())));
        }

        {
            let expected = LitExpr::Array(vec![
                LitExpr::Path(PathSelection {
                    path: PathList::Key(Key::Field("a".to_string()), Box::new(PathList::Empty)),
                }),
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::Field("b".to_string()),
                        Box::new(PathList::Key(
                            Key::Field("c".to_string()),
                            Box::new(PathList::Empty),
                        )),
                    ),
                }),
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::Field("d".to_string()),
                        Box::new(PathList::Key(
                            Key::Field("e".to_string()),
                            Box::new(PathList::Key(
                                Key::Field("f".to_string()),
                                Box::new(PathList::Empty),
                            )),
                        )),
                    ),
                }),
            ]);
            assert_eq!(
                LitExpr::parse("[.a, b.c, .d.e.f]"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("[.a, b.c, .d.e.f,]"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("[ . a , b . c , . d . e . f ]"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse("[ . a , b . c , . d . e . f , ]"),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse(
                    r#"[
                .a,
                b.c,
                .d.e.f,
            ]"#
                ),
                Ok(("", expected.clone()))
            );
            assert_eq!(
                LitExpr::parse(
                    r#"[
                . a ,
                . b . c ,
                d . e . f ,
            ]"#
                ),
                Ok(("", expected.clone()))
            );
        }

        {
            let expected = LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    "a".to_string(),
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            "$args".to_string(),
                            Box::new(PathList::Key(
                                Key::Field("a".to_string()),
                                Box::new(PathList::Empty),
                            )),
                        ),
                    }),
                );
                map.insert(
                    "b".to_string(),
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            "$this".to_string(),
                            Box::new(PathList::Key(
                                Key::Field("b".to_string()),
                                Box::new(PathList::Empty),
                            )),
                        ),
                    }),
                );
                map
            });

            assert_eq!(
                LitExpr::parse(
                    r#"{
                    a: $args.a,
                    b: $this.b,
                }"#
                ),
                Ok(("", expected.clone())),
            );

            assert_eq!(
                LitExpr::parse(
                    r#"{
                    b: $this.b,
                    a: $args.a,
                }"#
                ),
                Ok(("", expected.clone())),
            );

            assert_eq!(
                LitExpr::parse(
                    r#" {
                    a : $args . a ,
                    b : $this . b
                ,} "#
                ),
                Ok(("", expected.clone())),
            );
        }
    }
}
