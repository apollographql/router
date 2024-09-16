//! A LitExpr (short for LiteralExpression) is similar to a JSON value (or
//! serde_json::Value), with the addition of PathSelection as a possible leaf
//! value, so literal expressions passed to -> methods (via MethodArgs) can
//! incorporate dynamic $variable values in addition to the usual input data and
//! argument values.

use apollo_compiler::collections::IndexMap;
use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::multi::many1;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;

use super::helpers::spaces_or_comments;
use super::location::merge_ranges;
use super::location::ranged_span;
use super::location::Ranged;
use super::location::Span;
use super::location::WithRange;
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
    Object(IndexMap<WithRange<Key>, WithRange<LitExpr>>),
    Array(Vec<WithRange<LitExpr>>),
    Path(PathSelection),
}

impl LitExpr {
    // LitExpr      ::= LitPrimitive | LitObject | LitArray | PathSelection
    // LitPrimitive ::= LitString | LitNumber | "true" | "false" | "null"
    pub fn parse(input: Span) -> IResult<Span, WithRange<Self>> {
        tuple((
            spaces_or_comments,
            alt((
                map(parse_string_literal, |s| s.take_as(Self::String)),
                Self::parse_number,
                map(ranged_span("true"), |t| {
                    WithRange::new(Self::Bool(true), t.range())
                }),
                map(ranged_span("false"), |f| {
                    WithRange::new(Self::Bool(false), f.range())
                }),
                map(ranged_span("null"), |n| {
                    WithRange::new(Self::Null, n.range())
                }),
                Self::parse_object,
                Self::parse_array,
                map(PathSelection::parse_as_lit_expr, |p| {
                    let range = p.range();
                    WithRange::new(Self::Path(p), range)
                }),
            )),
        ))(input)
        .map(|(input, (_, value))| (input, value))
    }

    // LitNumber ::= "-"? ([0-9]+ ("." [0-9]*)? | "." [0-9]+)
    fn parse_number(input: Span) -> IResult<Span, WithRange<Self>> {
        let (suffix, (_, neg, _, num)) = tuple((
            spaces_or_comments,
            opt(ranged_span("-")),
            spaces_or_comments,
            alt((
                map(
                    pair(
                        recognize(many1(one_of("0123456789"))),
                        opt(tuple((
                            spaces_or_comments,
                            ranged_span("."),
                            spaces_or_comments,
                            recognize(many0(one_of("0123456789"))),
                        ))),
                    ),
                    |(int, frac)| {
                        let int_range = Some(
                            int.location_offset()..int.location_offset() + int.fragment().len(),
                        );

                        let mut s = String::new();
                        s.push_str(int.fragment());

                        let full_range = if let Some((_, dot, _, frac)) = frac {
                            let frac_range = merge_ranges(
                                dot.range(),
                                if frac.len() > 0 {
                                    Some(
                                        frac.location_offset()
                                            ..frac.location_offset() + frac.fragment().len(),
                                    )
                                } else {
                                    None
                                },
                            );
                            s.push('.');
                            if frac.fragment().is_empty() {
                                s.push('0');
                            } else {
                                s.push_str(frac.fragment());
                            }
                            merge_ranges(int_range, frac_range)
                        } else {
                            int_range
                        };

                        WithRange::new(s, full_range)
                    },
                ),
                map(
                    tuple((
                        spaces_or_comments,
                        ranged_span("."),
                        spaces_or_comments,
                        recognize(many1(one_of("0123456789"))),
                    )),
                    |(_, dot, _, frac)| {
                        let frac_range = Some(
                            frac.location_offset()..frac.location_offset() + frac.fragment().len(),
                        );
                        let full_range = merge_ranges(dot.range(), frac_range);
                        WithRange::new(format!("0.{}", frac.fragment()), full_range)
                    },
                ),
            )),
        ))(input)?;

        let mut number = String::new();
        if neg.is_some() {
            number.push('-');
        }
        number.push_str(num.as_str());

        if let Ok(lit_number) = number.parse().map(Self::Number) {
            let range = merge_ranges(neg.and_then(|n| n.range()), num.range());
            Ok((suffix, WithRange::new(lit_number, range)))
        } else {
            Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::IsNot,
            )))
        }
    }

    // LitObject ::= "{" (LitProperty ("," LitProperty)* ","?)? "}"
    fn parse_object(input: Span) -> IResult<Span, WithRange<Self>> {
        tuple((
            spaces_or_comments,
            ranged_span("{"),
            spaces_or_comments,
            map(
                opt(tuple((
                    Self::parse_property,
                    many0(preceded(
                        tuple((spaces_or_comments, char(','))),
                        Self::parse_property,
                    )),
                    opt(tuple((spaces_or_comments, char(',')))),
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
            spaces_or_comments,
            ranged_span("}"),
        ))(input)
        .map(|(input, (_, open_brace, _, output, _, close_brace))| {
            let range = merge_ranges(open_brace.range(), close_brace.range());
            (input, WithRange::new(output, range))
        })
    }

    // LitProperty ::= Key ":" LitExpr
    fn parse_property(input: Span) -> IResult<Span, (WithRange<Key>, WithRange<Self>)> {
        tuple((Key::parse, spaces_or_comments, char(':'), Self::parse))(input)
            .map(|(input, (key, _, _colon, value))| (input, (key, value)))
    }

    // LitArray ::= "[" (LitExpr ("," LitExpr)* ","?)? "]"
    fn parse_array(input: Span) -> IResult<Span, WithRange<Self>> {
        tuple((
            spaces_or_comments,
            ranged_span("["),
            spaces_or_comments,
            map(
                opt(tuple((
                    Self::parse,
                    many0(preceded(
                        tuple((spaces_or_comments, char(','))),
                        Self::parse,
                    )),
                    opt(tuple((spaces_or_comments, char(',')))),
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
            spaces_or_comments,
            ranged_span("]"),
        ))(input)
        .map(|(input, (_, open_bracket, _, output, _, close_bracket))| {
            let range = merge_ranges(open_bracket.range(), close_bracket.range());
            (input, WithRange::new(output, range))
        })
    }

    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
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
    use super::super::location::strip_ranges::StripRanges;
    use super::*;
    use crate::sources::connect::json_selection::helpers::span_is_all_spaces_or_comments;
    use crate::sources::connect::json_selection::PathList;

    fn check_parse(input: &str, expected: LitExpr) {
        match LitExpr::parse(Span::new(input)) {
            Ok((remainder, parsed)) => {
                assert!(span_is_all_spaces_or_comments(remainder));
                assert_eq!(parsed.strip_ranges(), WithRange::new(expected, None));
            }
            Err(e) => panic!("Failed to parse '{}': {:?}", input, e),
        };
    }

    #[test]
    fn test_lit_expr_parse_primitives() {
        check_parse("'hello'", LitExpr::String("hello".to_string()));
        check_parse("\"hello\"", LitExpr::String("hello".to_string()));
        check_parse(" 'hello' ", LitExpr::String("hello".to_string()));
        check_parse(" \"hello\" ", LitExpr::String("hello".to_string()));

        check_parse("123", LitExpr::Number(serde_json::Number::from(123)));
        check_parse("-123", LitExpr::Number(serde_json::Number::from(-123)));
        check_parse(" - 123 ", LitExpr::Number(serde_json::Number::from(-123)));
        check_parse(
            "123.456",
            LitExpr::Number(serde_json::Number::from_f64(123.456).unwrap()),
        );
        check_parse(
            ".456",
            LitExpr::Number(serde_json::Number::from_f64(0.456).unwrap()),
        );
        check_parse(
            "-.456",
            LitExpr::Number(serde_json::Number::from_f64(-0.456).unwrap()),
        );
        check_parse(
            "123.",
            LitExpr::Number(serde_json::Number::from_f64(123.0).unwrap()),
        );
        check_parse(
            "-123.",
            LitExpr::Number(serde_json::Number::from_f64(-123.0).unwrap()),
        );

        check_parse("true", LitExpr::Bool(true));
        check_parse(" true ", LitExpr::Bool(true));
        check_parse("false", LitExpr::Bool(false));
        check_parse(" false ", LitExpr::Bool(false));
        check_parse("null", LitExpr::Null);
        check_parse(" null ", LitExpr::Null);
    }

    #[test]
    fn test_lit_expr_parse_objects() {
        check_parse(
            "{a: 1}",
            LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    Key::field("a").into_with_range(),
                    LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                );
                map
            }),
        );

        check_parse(
            "{'a': 1}",
            LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    Key::quoted("a").into_with_range(),
                    LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                );
                map
            }),
        );

        {
            fn make_expected(a_key: Key, b_key: Key) -> LitExpr {
                let mut map = IndexMap::default();
                map.insert(
                    a_key.into_with_range(),
                    LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                );
                map.insert(
                    b_key.into_with_range(),
                    LitExpr::Number(serde_json::Number::from(2)).into_with_range(),
                );
                LitExpr::Object(map)
            }
            check_parse(
                "{'a': 1, 'b': 2}",
                make_expected(Key::quoted("a"), Key::quoted("b")),
            );
            check_parse(
                "{ a : 1, 'b': 2}",
                make_expected(Key::field("a"), Key::quoted("b")),
            );
            check_parse(
                "{ a : 1, b: 2}",
                make_expected(Key::field("a"), Key::field("b")),
            );
            check_parse(
                "{ \"a\" : 1, \"b\": 2 }",
                make_expected(Key::quoted("a"), Key::quoted("b")),
            );
            check_parse(
                "{ \"a\" : 1, b: 2 }",
                make_expected(Key::quoted("a"), Key::field("b")),
            );
            check_parse(
                "{ a : 1, \"b\": 2 }",
                make_expected(Key::field("a"), Key::quoted("b")),
            );
        }
    }

    #[test]
    fn test_lit_expr_parse_arrays() {
        check_parse(
            "[1, 2]",
            LitExpr::Array(vec![
                WithRange::new(LitExpr::Number(serde_json::Number::from(1)), None),
                WithRange::new(LitExpr::Number(serde_json::Number::from(2)), None),
            ]),
        );

        check_parse(
            "[1, true, 'three']",
            LitExpr::Array(vec![
                WithRange::new(LitExpr::Number(serde_json::Number::from(1)), None),
                WithRange::new(LitExpr::Bool(true), None),
                WithRange::new(LitExpr::String("three".to_string()), None),
            ]),
        );
    }

    #[test]
    fn test_lit_expr_parse_paths() {
        {
            let expected = LitExpr::Path(PathSelection {
                path: PathList::Key(
                    Key::field("a").into_with_range(),
                    PathList::Key(
                        Key::field("b").into_with_range(),
                        PathList::Key(
                            Key::field("c").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            });

            check_parse("a.b.c", expected.clone());
            check_parse(" a . b . c ", expected.clone());
        }

        {
            let expected = LitExpr::Path(PathSelection {
                path: PathList::Key(
                    Key::field("data").into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            });
            check_parse(".data", expected.clone());
            check_parse(" . data ", expected.clone());
        }

        {
            let expected = LitExpr::Array(vec![
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::field("a").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                })
                .into_with_range(),
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::field("b").into_with_range(),
                        PathList::Key(
                            Key::field("c").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                })
                .into_with_range(),
                LitExpr::Path(PathSelection {
                    path: PathList::Key(
                        Key::field("d").into_with_range(),
                        PathList::Key(
                            Key::field("e").into_with_range(),
                            PathList::Key(
                                Key::field("f").into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                })
                .into_with_range(),
            ]);

            check_parse("[.a, b.c, .d.e.f]", expected.clone());
            check_parse("[.a, b.c, .d.e.f,]", expected.clone());
            check_parse("[ . a , b . c , . d . e . f ]", expected.clone());
            check_parse("[ . a , b . c , . d . e . f , ]", expected.clone());
            check_parse(
                r#"[
                .a,
                b.c,
                .d.e.f,
            ]"#,
                expected.clone(),
            );
            check_parse(
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
                    Key::field("a").into_with_range(),
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            KnownVariable::Args.into_with_range(),
                            PathList::Key(
                                Key::field("a").into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    })
                    .into_with_range(),
                );
                map.insert(
                    Key::field("b").into_with_range(),
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            KnownVariable::This.into_with_range(),
                            PathList::Key(
                                Key::field("b").into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    })
                    .into_with_range(),
                );
                map
            });

            check_parse(
                r#"{
                a: $args.a,
                b: $this.b,
            }"#,
                expected.clone(),
            );

            check_parse(
                r#"{
                b: $this.b,
                a: $args.a,
            }"#,
                expected.clone(),
            );

            check_parse(
                r#" {
                a : $args . a ,
                b : $this . b
            ,} "#,
                expected.clone(),
            );
        }
    }
}
