//! A LitExpr (short for LiteralExpression) is similar to a JSON value (or
//! serde_json::Value), with the addition of PathSelection as a possible leaf
//! value, so literal expressions passed to -> methods (via MethodArgs) can
//! incorporate dynamic $variable values in addition to the usual input data and
//! argument values.

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

use super::helpers::spaces_or_comments;
use super::location::parsed_tag;
use super::location::Parsed;
use super::parser::parse_string_literal;
use super::parser::Key;
use super::parser::PathSelection;
use super::ExternalVarPaths;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum LitExpr {
    String(String),
    Number(serde_json::Number),
    Bool(bool),
    Null,
    Object(IndexMap<Parsed<Key>, Parsed<LitExpr>>),
    Array(Vec<Parsed<LitExpr>>),
    Path(PathSelection),
}

impl LitExpr {
    // LitExpr      ::= LitPrimitive | LitObject | LitArray | PathSelection
    // LitPrimitive ::= LitString | LitNumber | "true" | "false" | "null"
    pub fn parse(input: &str) -> IResult<&str, Parsed<Self>> {
        tuple((
            spaces_or_comments,
            alt((
                map(parse_string_literal, |s| s.take_as(Self::String)),
                Self::parse_number,
                map(parsed_tag("true"), |t| {
                    Parsed::new(Self::Bool(true), t.loc())
                }),
                map(parsed_tag("false"), |f| {
                    Parsed::new(Self::Bool(false), f.loc())
                }),
                map(parsed_tag("null"), |n| Parsed::new(Self::Null, n.loc())),
                Self::parse_object,
                Self::parse_array,
                map(PathSelection::parse, |path| path.take_as(Self::Path)),
            )),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, value, _))| (input, value))
    }

    // LitNumber ::= "-"? ([0-9]+ ("." [0-9]*)? | "." [0-9]+)
    fn parse_number(input: &str) -> IResult<&str, Parsed<Self>> {
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
            Ok((suffix, Parsed::new(lit_number, None)))
        } else {
            Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::IsNot,
            )))
        }
    }

    // LitObject ::= "{" (LitProperty ("," LitProperty)* ","?)? "}"
    fn parse_object(input: &str) -> IResult<&str, Parsed<Self>> {
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
        .map(|(input, output)| (input, Parsed::new(output, None)))
    }

    // LitProperty ::= Key ":" LitExpr
    fn parse_property(input: &str) -> IResult<&str, (Parsed<Key>, Parsed<Self>)> {
        tuple((Key::parse, char(':'), Self::parse))(input)
            .map(|(input, (key, _, value))| (input, (key, value)))
    }

    // LitArray ::= "[" (LitExpr ("," LitExpr)* ","?)? "]"
    fn parse_array(input: &str) -> IResult<&str, Parsed<Self>> {
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
        .map(|(input, output)| (input, Parsed::new(output, None)))
    }

    pub(super) fn into_parsed(self) -> Parsed<Self> {
        Parsed::new(self, None)
    }

    pub(super) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(n) => n.as_i64(),
            _ => None,
        }
    }
}

impl ExternalVarPaths for LitExpr {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = vec![];
        match self {
            Self::String(_) | Self::Number(_) | Self::Bool(_) | Self::Null => {}
            Self::Object(map) => {
                for value in map.values() {
                    paths.extend(value.external_var_paths());
                }
            }
            Self::Array(vec) => {
                for value in vec {
                    paths.extend(value.external_var_paths());
                }
            }
            Self::Path(path) => {
                paths.extend(path.external_var_paths());
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::super::known_var::KnownVariable;
    use super::*;
    use crate::sources::connect::json_selection::location::StripLoc;
    use crate::sources::connect::json_selection::PathList;

    #[test]
    fn test_lit_expr_parse_primitives() {
        assert_eq!(
            LitExpr::parse("'hello'"),
            Ok(("", Parsed::new(LitExpr::String("hello".to_string()), None))),
        );
        assert_eq!(
            LitExpr::parse("\"hello\""),
            Ok(("", Parsed::new(LitExpr::String("hello".to_string()), None))),
        );

        assert_eq!(
            LitExpr::parse("123"),
            Ok((
                "",
                Parsed::new(LitExpr::Number(serde_json::Number::from(123)), None)
            )),
        );
        assert_eq!(
            LitExpr::parse("-123"),
            Ok((
                "",
                Parsed::new(LitExpr::Number(serde_json::Number::from(-123)), None)
            )),
        );
        assert_eq!(
            LitExpr::parse(" - 123 "),
            Ok((
                "",
                Parsed::new(LitExpr::Number(serde_json::Number::from(-123)), None)
            )),
        );
        assert_eq!(
            LitExpr::parse("123.456"),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Number(serde_json::Number::from_f64(123.456).unwrap()),
                    None
                ),
            )),
        );
        assert_eq!(
            LitExpr::parse(".456"),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Number(serde_json::Number::from_f64(0.456).unwrap()),
                    None
                ),
            )),
        );
        assert_eq!(
            LitExpr::parse("-.456"),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Number(serde_json::Number::from_f64(-0.456).unwrap()),
                    None
                ),
            )),
        );
        assert_eq!(
            LitExpr::parse("123."),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Number(serde_json::Number::from_f64(123.0).unwrap()),
                    None
                ),
            )),
        );
        assert_eq!(
            LitExpr::parse("-123."),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Number(serde_json::Number::from_f64(-123.0).unwrap()),
                    None
                ),
            )),
        );

        assert_eq!(
            LitExpr::parse("true"),
            Ok(("", Parsed::new(LitExpr::Bool(true), None)))
        );
        assert_eq!(
            LitExpr::parse(" true "),
            Ok(("", Parsed::new(LitExpr::Bool(true), None)))
        );
        assert_eq!(
            LitExpr::parse("false"),
            Ok(("", Parsed::new(LitExpr::Bool(false), None)))
        );
        assert_eq!(
            LitExpr::parse(" false "),
            Ok(("", Parsed::new(LitExpr::Bool(false), None)))
        );
        assert_eq!(
            LitExpr::parse("null"),
            Ok(("", Parsed::new(LitExpr::Null, None)))
        );
        assert_eq!(
            LitExpr::parse(" null "),
            Ok(("", Parsed::new(LitExpr::Null, None)))
        );
    }

    #[test]
    fn test_lit_expr_parse_objects() {
        assert_eq!(
            LitExpr::parse("{'a': 1}"),
            Ok((
                "",
                Parsed::new(
                    LitExpr::Object({
                        let mut map = IndexMap::default();
                        map.insert(
                            Parsed::new(Key::quoted("a"), None),
                            Parsed::new(LitExpr::Number(serde_json::Number::from(1)), None),
                        );
                        map
                    }),
                    None
                ),
            ))
        );

        {
            fn make_expected<'a>(a_key: Key, b_key: Key) -> IResult<&'a str, Parsed<LitExpr>> {
                let mut map = IndexMap::default();
                map.insert(
                    Parsed::new(a_key, None),
                    Parsed::new(LitExpr::Number(serde_json::Number::from(1)), None),
                );
                map.insert(
                    Parsed::new(b_key, None),
                    Parsed::new(LitExpr::Number(serde_json::Number::from(2)), None),
                );
                Ok(("", Parsed::new(LitExpr::Object(map), None)))
            }
            assert_eq!(
                LitExpr::parse("{'a': 1, 'b': 2}"),
                make_expected(Key::quoted("a"), Key::quoted("b")),
            );
            assert_eq!(
                LitExpr::parse("{ a : 1, 'b': 2}"),
                make_expected(Key::field("a"), Key::quoted("b")),
            );
            assert_eq!(
                LitExpr::parse("{ a : 1, b: 2}"),
                make_expected(Key::field("a"), Key::field("b")),
            );
            assert_eq!(
                LitExpr::parse("{ \"a\" : 1, \"b\": 2 }"),
                make_expected(Key::quoted("a"), Key::quoted("b")),
            );
            assert_eq!(
                LitExpr::parse("{ \"a\" : 1, b: 2 }"),
                make_expected(Key::quoted("a"), Key::field("b")),
            );
            assert_eq!(
                LitExpr::parse("{ a : 1, \"b\": 2 }"),
                make_expected(Key::field("a"), Key::quoted("b")),
            );
        }
    }

    fn check(input: &str, expected: LitExpr) {
        match LitExpr::parse(input) {
            Ok((remainder, parsed)) => {
                assert_eq!(remainder, "");
                assert_eq!(parsed.strip_loc(), Parsed::new(expected, None));
            }
            Err(e) => panic!("Failed to parse '{}': {:?}", input, e),
        };
    }

    #[test]
    fn test_lit_expr_parse_arrays() {
        check(
            "[1, 2]",
            LitExpr::Array(vec![
                Parsed::new(LitExpr::Number(serde_json::Number::from(1)), None),
                Parsed::new(LitExpr::Number(serde_json::Number::from(2)), None),
            ]),
        );

        check(
            "[1, true, 'three']",
            LitExpr::Array(vec![
                Parsed::new(LitExpr::Number(serde_json::Number::from(1)), None),
                Parsed::new(LitExpr::Bool(true), None),
                Parsed::new(LitExpr::String("three".to_string()), None),
            ]),
        );
    }

    #[test]
    fn test_lit_expr_parse_paths() {
        {
            let expected = LitExpr::Path(PathSelection {
                path: Parsed::new(
                    PathList::Key(
                        Parsed::new(Key::Field("a".to_string()), None),
                        Parsed::new(
                            PathList::Key(
                                Parsed::new(Key::Field("b".to_string()), None),
                                Parsed::new(
                                    PathList::Key(
                                        Parsed::new(Key::Field("c".to_string()), None),
                                        Parsed::new(PathList::Empty, None),
                                    ),
                                    None,
                                ),
                            ),
                            None,
                        ),
                    ),
                    None,
                ),
            });
            check("a.b.c", expected.clone());
            check(" a . b . c ", expected.clone());
        }

        {
            let expected = LitExpr::Path(PathSelection {
                path: Parsed::new(
                    PathList::Key(
                        Parsed::new(Key::Field("data".to_string()), None),
                        Parsed::new(PathList::Empty, None),
                    ),
                    None,
                ),
            });
            check(".data", expected.clone());
            check(" . data ", expected.clone());
        }

        {
            let expected = LitExpr::Array(vec![
                Parsed::new(
                    LitExpr::Path(PathSelection {
                        path: Parsed::new(
                            PathList::Key(
                                Parsed::new(Key::Field("a".to_string()), None),
                                Parsed::new(PathList::Empty, None),
                            ),
                            None,
                        ),
                    }),
                    None,
                ),
                Parsed::new(
                    LitExpr::Path(PathSelection {
                        path: Parsed::new(
                            PathList::Key(
                                Parsed::new(Key::Field("b".to_string()), None),
                                Parsed::new(
                                    PathList::Key(
                                        Parsed::new(Key::Field("c".to_string()), None),
                                        Parsed::new(PathList::Empty, None),
                                    ),
                                    None,
                                ),
                            ),
                            None,
                        ),
                    }),
                    None,
                ),
                Parsed::new(
                    LitExpr::Path(PathSelection {
                        path: Parsed::new(
                            PathList::Key(
                                Parsed::new(Key::Field("d".to_string()), None),
                                Parsed::new(
                                    PathList::Key(
                                        Parsed::new(Key::Field("e".to_string()), None),
                                        Parsed::new(
                                            PathList::Key(
                                                Parsed::new(Key::Field("f".to_string()), None),
                                                Parsed::new(PathList::Empty, None),
                                            ),
                                            None,
                                        ),
                                    ),
                                    None,
                                ),
                            ),
                            None,
                        ),
                    }),
                    None,
                ),
            ]);
            check("[.a, b.c, .d.e.f]", expected.clone());
            check("[.a, b.c, .d.e.f,]", expected.clone());
            check("[ . a , b . c , . d . e . f ]", expected.clone());
            check("[ . a , b . c , . d . e . f , ]", expected.clone());
            check(
                r#"[
                .a,
                b.c,
                .d.e.f,
            ]"#,
                expected.clone(),
            );
            check(
                r#"[
                . a ,
                . b . c ,
                d . e . f ,
            ]"#,
                expected.clone(),
            );
        }

        {
            let expected = LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    Parsed::new(Key::Field("a".to_string()), None),
                    Parsed::new(
                        LitExpr::Path(PathSelection {
                            path: Parsed::new(
                                PathList::Var(
                                    Parsed::new(KnownVariable::Args, None),
                                    Parsed::new(
                                        PathList::Key(
                                            Parsed::new(Key::Field("a".to_string()), None),
                                            Parsed::new(PathList::Empty, None),
                                        ),
                                        None,
                                    ),
                                ),
                                None,
                            ),
                        }),
                        None,
                    ),
                );
                map.insert(
                    Parsed::new(Key::Field("b".to_string()), None),
                    Parsed::new(
                        LitExpr::Path(PathSelection {
                            path: Parsed::new(
                                PathList::Var(
                                    Parsed::new(KnownVariable::This, None),
                                    Parsed::new(
                                        PathList::Key(
                                            Parsed::new(Key::Field("b".to_string()), None),
                                            Parsed::new(PathList::Empty, None),
                                        ),
                                        None,
                                    ),
                                ),
                                None,
                            ),
                        }),
                        None,
                    ),
                );
                map
            });

            check(
                r#"{
                a: $args.a,
                b: $this.b,
            }"#,
                expected.clone(),
            );

            check(
                r#"{
                b: $this.b,
                a: $args.a,
            }"#,
                expected.clone(),
            );

            check(
                r#" {
                a : $args . a ,
                b : $this . b
            ,} "#,
                expected.clone(),
            );
        }
    }
}
