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

use super::ParseResult;
use super::PathList;
use super::VarPaths;
use super::helpers::spaces_or_comments;
use super::location::Ranged;
use super::location::Span;
use super::location::WithRange;
use super::location::merge_ranges;
use super::location::ranged_span;
use super::nom_error_message;
use super::parser::Key;
use super::parser::PathSelection;
use super::parser::nom_fail_message;
use super::parser::parse_string_literal;
use crate::connectors::spec::ConnectSpec;

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum LitExpr {
    String(String),
    Number(serde_json::Number),
    Bool(bool),
    Null,
    Object(IndexMap<WithRange<Key>, WithRange<LitExpr>>),
    Array(Vec<WithRange<LitExpr>>),
    Path(PathSelection),

    // Whereas the LitExpr::Path variant wraps a PathSelection that obeys the
    // parsing rules of the outer selection syntax (i.e. default JSONSelection
    // syntax, not LitExpr syntax), this LitExpr::LitPath variant can be parsed
    // only as part of a LitExpr, and allows the value at the root of the path
    // to be any LitExpr literal expression, without needing a $(...) wrapper,
    // allowing you to write "asdf"->slice(0, 2) when you're already in an
    // expression parsing context, rather than $(asdf)->slice(0, 2).
    //
    // The WithRange<LitExpr> argument is the root expression (never a
    // LitExpr::Path), and the WithRange<PathList> argument represents the rest
    // of the path, which is never PathList::Empty, because that would mean the
    // LitExpr could stand on its own, using one of the other variants.
    LitPath(WithRange<LitExpr>, WithRange<PathList>),

    // Operator chains: A op B op C ... where all operators are the same type
    // OpChain contains the operator type and a vector of operands
    // For example: A ?? B ?? C becomes OpChain(NullishCoalescing, [A, B, C])
    OpChain(WithRange<LitOp>, Vec<WithRange<LitExpr>>),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum LitOp {
    NullishCoalescing, // ??
    NoneCoalescing,    // ?!
}

impl LitOp {
    #[cfg(test)]
    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
    }

    pub(super) fn as_str(&self) -> &str {
        match self {
            LitOp::NullishCoalescing => "??",
            LitOp::NoneCoalescing => "?!",
        }
    }
}

impl LitExpr {
    // LitExpr ::= LitOpChain | LitPath | LitPrimitive | LitObject | LitArray | PathSelection
    pub(crate) fn parse(input: Span) -> ParseResult<WithRange<Self>> {
        match input.extra.spec {
            ConnectSpec::V0_1 | ConnectSpec::V0_2 => {
                let (input, _) = spaces_or_comments(input)?;
                Self::parse_primary(input)
            }
            ConnectSpec::V0_3 | ConnectSpec::V0_4 => Self::parse_with_operators(input),
        }
    }

    // Parse expressions with operator chains (no precedence since we forbid mixing operators)
    fn parse_with_operators(input: Span) -> ParseResult<WithRange<Self>> {
        let (input, _) = spaces_or_comments(input)?;

        // Parse the left-hand side (primary expression)
        let (mut input, left) = Self::parse_primary(input)?;

        // Track operators and operands for building OpChain
        let mut current_op: Option<WithRange<LitOp>> = None;
        let mut operands = vec![left.clone()];

        loop {
            let (input_after_spaces, _) = spaces_or_comments(input.clone())?;

            // Try to parse a binary operator
            if let Ok((suffix, op)) = Self::parse_binary_operator(input_after_spaces.clone()) {
                // Check if we're starting a new operator chain or continuing an existing one
                match current_op {
                    None => {
                        // Starting a new operator chain
                        current_op = Some(op);
                    }
                    Some(ref existing_op) if existing_op.as_ref() != op.as_ref() => {
                        // Operator mismatch - we cannot mix operators in a chain
                        // This breaks the chain, so we need to stop parsing here
                        let err = format!(
                            "Found mixed operators {} and {}. You can only chain operators of the same kind.",
                            existing_op.as_str(),
                            op.as_str(),
                        );
                        return Err(nom_fail_message(input_after_spaces, err));
                    }
                    Some(_) => {
                        // Same operator, continue the chain
                    }
                }

                // Parse the right-hand side (with spaces)
                let (suffix_with_spaces, _) = spaces_or_comments(suffix)?;
                let (remainder, right) = Self::parse_primary(suffix_with_spaces)?;

                operands.push(right);
                input = remainder;
            } else {
                break;
            }
        }

        // Build the final expression
        let result = if let Some(op) = current_op {
            let full_range = if operands.len() >= 2 {
                merge_ranges(
                    operands.first().and_then(|o| o.range()),
                    operands.last().and_then(|o| o.range()),
                )
            } else {
                operands.first().and_then(|o| o.range())
            };
            WithRange::new(Self::OpChain(op, operands), full_range)
        } else {
            left
        };

        Ok((input, result))
    }

    fn parse_primary(input: Span) -> ParseResult<WithRange<Self>> {
        match alt((Self::parse_primitive, Self::parse_object, Self::parse_array))(input.clone()) {
            Ok((suffix, initial_literal)) => {
                // If we parsed an initial literal expression, it may be the
                // entire result, but we also want to greedily parse one or more
                // PathStep items that follow it, according to the rule
                //
                //    LitPath ::= (LitPrimitive | LitObject | LitArray) PathStep+
                //
                // This allows paths beginning with literal values without the
                // initial $(...) expression wrapper, so you can write
                // $(123->add(111)) instead of $($(123)->add(111)) when you're
                // already in a LitExpr parsing context.
                //
                // We begin parsing the path at depth 1 rather than 0 because
                // we've already parsed the initial literal at depth 0, so the
                // subpath should obey the parsing rules for for depth > 0.
                match PathList::parse_with_depth(suffix.clone(), 1) {
                    Ok((remainder, subpath)) => {
                        if matches!(subpath.as_ref(), PathList::Empty) {
                            return Ok((remainder, initial_literal));
                        }
                        let full_range = merge_ranges(initial_literal.range(), subpath.range());
                        Ok((
                            remainder,
                            WithRange::new(Self::LitPath(initial_literal, subpath), full_range),
                        ))
                    }
                    // If we failed to parse a path, return initial_literal as-is.
                    Err(_) => Ok((suffix.clone(), initial_literal)),
                }
            }

            // If we failed to parse a primitive, object, or array, try parsing
            // a PathSelection (which cannot be a LitPath).
            Err(_) => PathSelection::parse(input.clone()).map(|(remainder, path)| {
                let range = path.range();
                (remainder, WithRange::new(Self::Path(path), range))
            }),
        }
    }

    fn parse_binary_operator(input: Span) -> ParseResult<WithRange<LitOp>> {
        alt((
            map(ranged_span("??"), |qq| {
                WithRange::new(LitOp::NullishCoalescing, qq.range())
            }),
            map(ranged_span("?!"), |qq| {
                WithRange::new(LitOp::NoneCoalescing, qq.range())
            }),
        ))(input)
    }

    // LitPrimitive ::= LitString | LitNumber | "true" | "false" | "null"
    fn parse_primitive(input: Span) -> ParseResult<WithRange<Self>> {
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
        ))(input)
    }

    // LitNumber ::= "-"? ([0-9]+ ("." [0-9]*)? | "." [0-9]+)
    fn parse_number(input: Span) -> ParseResult<WithRange<Self>> {
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

                        // Remove leading zeros to avoid failing the stricter
                        // number.parse() below, but allow a single zero.
                        let mut int_chars_without_leading_zeros =
                            int.fragment().chars().skip_while(|c| *c == '0');
                        if let Some(first_non_zero) = int_chars_without_leading_zeros.next() {
                            s.push(first_non_zero);
                            s.extend(int_chars_without_leading_zeros);
                        } else {
                            s.push('0');
                        }

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
        ))(input.clone())?;

        let mut number = String::new();
        if neg.is_some() {
            number.push('-');
        }
        number.push_str(num.as_str());

        number.parse().map(Self::Number).map_or_else(
            |_| {
                // CONSIDER USING THIS ERROR? now that we have access to them?
                Err(nom_error_message(
                    input,
                    // We could include the faulty number in the error message, but
                    // it will also appear at the beginning of the input span.
                    "Failed to parse numeric literal",
                ))
            },
            |lit_number| {
                Ok((
                    suffix,
                    WithRange::new(
                        lit_number,
                        merge_ranges(neg.and_then(|n| n.range()), num.range()),
                    ),
                ))
            },
        )
    }

    // LitObject ::= "{" (LitProperty ("," LitProperty)* ","?)? "}"
    fn parse_object(input: Span) -> ParseResult<WithRange<Self>> {
        let (input, _) = spaces_or_comments(input)?;
        let (input, open_brace) = ranged_span("{")(input)?;
        let (mut input, _) = spaces_or_comments(input)?;

        let mut output = IndexMap::default();

        if let Ok((remainder, (key, value))) = Self::parse_property(input.clone()) {
            output.insert(key, value);
            input = remainder;

            while let Ok((remainder, _)) = tuple((spaces_or_comments, char(',')))(input.clone()) {
                input = remainder;
                if let Ok((remainder, (key, value))) = Self::parse_property(input.clone()) {
                    output.insert(key, value);
                    input = remainder;
                } else {
                    break;
                }
            }
        }

        let (input, _) = spaces_or_comments(input.clone())?;
        let (input, close_brace) = ranged_span("}")(input)?;

        let range = merge_ranges(open_brace.range(), close_brace.range());
        Ok((input, WithRange::new(Self::Object(output), range)))
    }

    // LitProperty ::= Key ":" LitExpr
    fn parse_property(input: Span) -> ParseResult<(WithRange<Key>, WithRange<Self>)> {
        tuple((Key::parse, spaces_or_comments, char(':'), Self::parse))(input)
            .map(|(input, (key, _, _colon, value))| (input, (key, value)))
    }

    // LitArray ::= "[" (LitExpr ("," LitExpr)* ","?)? "]"
    fn parse_array(input: Span) -> ParseResult<WithRange<Self>> {
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

    #[cfg(test)]
    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
    }

    #[allow(unused)]
    pub(super) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(n) => n.as_i64(),
            _ => None,
        }
    }
}

impl VarPaths for LitExpr {
    fn var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = vec![];
        match self {
            Self::String(_) | Self::Number(_) | Self::Bool(_) | Self::Null => {}
            Self::Object(map) => {
                for value in map.values() {
                    paths.extend(value.var_paths());
                }
            }
            Self::Array(vec) => {
                for value in vec {
                    paths.extend(value.var_paths());
                }
            }
            Self::Path(path) => {
                paths.extend(path.var_paths());
            }
            Self::LitPath(literal, subpath) => {
                paths.extend(literal.var_paths());
                paths.extend(subpath.var_paths());
            }
            Self::OpChain(_, operands) => {
                for operand in operands {
                    paths.extend(operand.var_paths());
                }
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
    use crate::connectors::json_selection::MethodArgs;
    use crate::connectors::json_selection::PathList;
    use crate::connectors::json_selection::PrettyPrintable;
    use crate::connectors::json_selection::fixtures::Namespace;
    use crate::connectors::json_selection::helpers::span_is_all_spaces_or_comments;
    use crate::connectors::json_selection::location::new_span;
    use crate::connectors::json_selection::location::new_span_with_spec;
    use crate::connectors::spec::ConnectSpec;

    #[track_caller]
    fn check_parse(input: &str, expected: LitExpr) {
        match LitExpr::parse(new_span(input)) {
            Ok((remainder, parsed)) => {
                assert!(span_is_all_spaces_or_comments(remainder));
                assert_eq!(parsed.strip_ranges(), WithRange::new(expected, None));
            }
            Err(e) => panic!("Failed to parse '{input}': {e:?}"),
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
        check_parse("00", LitExpr::Number(serde_json::Number::from(0)));
        check_parse(
            "-00",
            LitExpr::Number(serde_json::Number::from_f64(-0.0).unwrap()),
        );
        check_parse("0", LitExpr::Number(serde_json::Number::from(0)));
        check_parse(
            "-0",
            LitExpr::Number(serde_json::Number::from_f64(-0.0).unwrap()),
        );
        check_parse(" 00 ", LitExpr::Number(serde_json::Number::from(0)));
        check_parse(" 0 ", LitExpr::Number(serde_json::Number::from(0)));
        check_parse(
            " - 0 ",
            LitExpr::Number(serde_json::Number::from_f64(-0.0).unwrap()),
        );
        check_parse("001", LitExpr::Number(serde_json::Number::from(1)));
        check_parse(
            "00.1",
            LitExpr::Number(serde_json::Number::from_f64(0.1).unwrap()),
        );
        check_parse("0010", LitExpr::Number(serde_json::Number::from(10)));
        check_parse(
            "00.10",
            LitExpr::Number(serde_json::Number::from_f64(0.1).unwrap()),
        );
        check_parse("-001 ", LitExpr::Number(serde_json::Number::from(-1)));
        check_parse(
            "-00.1",
            LitExpr::Number(serde_json::Number::from_f64(-0.1).unwrap()),
        );
        check_parse(" - 0010 ", LitExpr::Number(serde_json::Number::from(-10)));
        check_parse(
            "- 00.10",
            LitExpr::Number(serde_json::Number::from_f64(-0.1).unwrap()),
        );
        check_parse(
            "007.",
            LitExpr::Number(serde_json::Number::from_f64(7.0).unwrap()),
        );
        check_parse(
            "-007.",
            LitExpr::Number(serde_json::Number::from_f64(-7.0).unwrap()),
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
            check_parse(" a . b . c ", expected);
        }

        {
            let expected = LitExpr::Path(PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("data").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            });
            check_parse("$.data", expected.clone());
            check_parse(" $ . data ", expected);
        }

        {
            let expected = LitExpr::Array(vec![
                LitExpr::Path(PathSelection {
                    path: PathList::Var(
                        KnownVariable::Dollar.into_with_range(),
                        PathList::Key(
                            Key::field("a").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
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

            check_parse("[$.a, b.c, d.e.f]", expected.clone());
            check_parse("[$.a, b.c, d.e.f,]", expected.clone());
            check_parse("[ $ . a , b . c , d . e . f ]", expected.clone());
            check_parse("[ $ . a , b . c , d . e . f , ]", expected.clone());
            check_parse(
                r#"[
                $.a,
                b.c,
                d.e.f,
            ]"#,
                expected.clone(),
            );
            check_parse(
                r#"[
                $ . a ,
                b . c ,
                d . e . f ,
            ]"#,
                expected,
            );
        }

        {
            let expected = LitExpr::Object({
                let mut map = IndexMap::default();
                map.insert(
                    Key::field("a").into_with_range(),
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            KnownVariable::External(Namespace::Args.to_string()).into_with_range(),
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
                            KnownVariable::External(Namespace::This.to_string()).into_with_range(),
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
                expected,
            );
        }
    }

    #[test]
    fn test_literal_methods() {
        #[track_caller]
        fn check_parse_and_print(input: &str, expected: LitExpr) {
            let expected_inline = expected.pretty_print_with_indentation(true, 0);
            match LitExpr::parse(new_span(input)) {
                Ok((remainder, parsed)) => {
                    assert!(span_is_all_spaces_or_comments(remainder));
                    assert_eq!(parsed.strip_ranges(), WithRange::new(expected, None));
                    assert_eq!(parsed.pretty_print_with_indentation(true, 0), input);
                    assert_eq!(expected_inline, input);
                }
                Err(e) => panic!("Failed to parse '{input}': {e:?}"),
            };
        }

        check_parse_and_print(
            "$(\"a\")->first",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::String("a".to_string()).into_with_range(),
                    PathList::Method(
                        WithRange::new("first".to_string(), None),
                        None,
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(\"a\"->first)",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::LitPath(
                        LitExpr::String("a".to_string()).into_with_range(),
                        PathList::Method(
                            WithRange::new("first".to_string(), None),
                            None,
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(1234)->add(1111)",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Number(serde_json::Number::from(1234)).into_with_range(),
                    PathList::Method(
                        WithRange::new("add".to_string(), None),
                        Some(MethodArgs {
                            args: vec![
                                LitExpr::Number(serde_json::Number::from(1111)).into_with_range(),
                            ],
                            range: None,
                        }),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(1234->add(1111))",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::LitPath(
                        LitExpr::Number(serde_json::Number::from(1234)).into_with_range(),
                        PathList::Method(
                            WithRange::new("add".to_string(), None),
                            Some(MethodArgs {
                                args: vec![
                                    LitExpr::Number(serde_json::Number::from(1111))
                                        .into_with_range(),
                                ],
                                range: None,
                            }),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(value->mul(10))",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("value").into_with_range(),
                            PathList::Method(
                                WithRange::new("mul".to_string(), None),
                                Some(MethodArgs {
                                    args: vec![
                                        LitExpr::Number(serde_json::Number::from(10))
                                            .into_with_range(),
                                    ],
                                    range: None,
                                }),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    })
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(value.key->typeof)",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("value").into_with_range(),
                            PathList::Key(
                                Key::field("key").into_with_range(),
                                PathList::Method(
                                    WithRange::new("typeof".to_string(), None),
                                    None,
                                    PathList::Empty.into_with_range(),
                                )
                                .into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    })
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$(value.key)->typeof",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Path(PathSelection {
                        path: PathList::Key(
                            Key::field("value").into_with_range(),
                            PathList::Key(
                                Key::field("key").into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    })
                    .into_with_range(),
                    PathList::Method(
                        WithRange::new("typeof".to_string(), None),
                        None,
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$([1, 2, 3])->last",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Array(vec![
                        LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                        LitExpr::Number(serde_json::Number::from(2)).into_with_range(),
                        LitExpr::Number(serde_json::Number::from(3)).into_with_range(),
                    ])
                    .into_with_range(),
                    PathList::Method(
                        WithRange::new("last".to_string(), None),
                        None,
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$([1, 2, 3]->last)",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::LitPath(
                        LitExpr::Array(vec![
                            LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                            LitExpr::Number(serde_json::Number::from(2)).into_with_range(),
                            LitExpr::Number(serde_json::Number::from(3)).into_with_range(),
                        ])
                        .into_with_range(),
                        PathList::Method(
                            WithRange::new("last".to_string(), None),
                            None,
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$({ a: \"ay\", b: 1 }).a",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::Object({
                        let mut map = IndexMap::default();
                        map.insert(
                            Key::field("a").into_with_range(),
                            LitExpr::String("ay".to_string()).into_with_range(),
                        );
                        map.insert(
                            Key::field("b").into_with_range(),
                            LitExpr::Number(serde_json::Number::from(1)).into_with_range(),
                        );
                        map
                    })
                    .into_with_range(),
                    PathList::Key(
                        Key::field("a").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        check_parse_and_print(
            "$({ a: \"ay\", b: 2 }.a)",
            LitExpr::Path(PathSelection {
                path: PathList::Expr(
                    LitExpr::LitPath(
                        LitExpr::Object({
                            let mut map = IndexMap::default();
                            map.insert(
                                Key::field("a").into_with_range(),
                                LitExpr::String("ay".to_string()).into_with_range(),
                            );
                            map.insert(
                                Key::field("b").into_with_range(),
                                LitExpr::Number(serde_json::Number::from(2)).into_with_range(),
                            );
                            map
                        })
                        .into_with_range(),
                        PathList::Key(
                            Key::field("a").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            }),
        );
    }

    #[test]
    fn test_null_coalescing_operator_parsing() {
        // Test basic parsing
        check_parse_with_spec(
            "null ?? 'Bar'",
            ConnectSpec::V0_3,
            LitExpr::OpChain(
                LitOp::NullishCoalescing.into_with_range(),
                vec![
                    LitExpr::Null.into_with_range(),
                    LitExpr::String("Bar".to_string()).into_with_range(),
                ],
            ),
        );

        check_parse_with_spec(
            "null ?! 'Bar'",
            ConnectSpec::V0_3,
            LitExpr::OpChain(
                LitOp::NoneCoalescing.into_with_range(),
                vec![
                    LitExpr::Null.into_with_range(),
                    LitExpr::String("Bar".to_string()).into_with_range(),
                ],
            ),
        );
    }

    #[test]
    fn test_null_coalescing_chaining() {
        // Test chaining: A ?? B ?? C should parse as OpChain(NullishCoalescing, [A, B, C])
        check_parse_with_spec(
            "null ?? null ?? 'Bar'",
            ConnectSpec::V0_3,
            LitExpr::OpChain(
                LitOp::NullishCoalescing.into_with_range(),
                vec![
                    LitExpr::Null.into_with_range(),
                    LitExpr::Null.into_with_range(),
                    LitExpr::String("Bar".to_string()).into_with_range(),
                ],
            ),
        );
    }

    #[test]
    fn test_operator_mixing_validation() {
        // Test that mixing operators in a chain fails to parse
        let result = LitExpr::parse(new_span_with_spec(
            "null ?? 'foo' ?! 'bar'",
            ConnectSpec::V0_3,
        ));

        // Should fail with mixed operators error
        let err = result.expect_err("Expected parse error for mixed operators ?? and ?!");

        // Verify the error message contains information about mixed operators
        let error_msg = format!("{err:?}");
        assert!(
            error_msg.contains("Found mixed operators ?? and ?!"),
            "Expected mixed operators error message, got: {error_msg}"
        );
    }

    #[track_caller]
    fn check_parse_with_spec(input: &str, spec: ConnectSpec, expected: LitExpr) {
        match LitExpr::parse(new_span_with_spec(input, spec)) {
            Ok((remainder, parsed)) => {
                assert!(span_is_all_spaces_or_comments(remainder));
                assert_eq!(parsed.strip_ranges(), WithRange::new(expected, None));
            }
            Err(e) => panic!("Failed to parse '{input}': {e:?}"),
        }
    }
}
