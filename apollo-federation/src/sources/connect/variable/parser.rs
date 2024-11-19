//! Parsing functions for variable references.

use std::borrow::Cow;
use std::ops::Range;
use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::alpha1;
use nom::character::complete::alphanumeric1;
use nom::character::complete::char;
use nom::combinator::map;
use nom::combinator::recognize;
use nom::error::Error;
use nom::error::ErrorKind;
use nom::error::ParseError;
use nom::multi::many0;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use nom_locate::LocatedSpan;

use crate::sources::connect::variable::VariableNamespace;
use crate::sources::connect::variable::VariablePathPart;
use crate::sources::connect::variable::VariableReference;

pub(crate) type Span<'a> = LocatedSpan<&'a str>;

#[derive(Debug, PartialEq)]
pub(crate) enum VariableParseError<I> {
    InvalidNamespace {
        namespace: String,
        location: Range<usize>,
    },
    Nom(I, ErrorKind),
}

impl<I> ParseError<I> for VariableParseError<I> {
    fn from_error_kind(input: I, kind: ErrorKind) -> Self {
        VariableParseError::Nom(input, kind)
    }

    fn append(_: I, _: ErrorKind, other: Self) -> Self {
        other
    }
}

impl<I> From<nom::Err<Error<I>>> for VariableParseError<I> {
    fn from(value: nom::Err<Error<I>>) -> Self {
        match value {
            nom::Err::Error(e) | nom::Err::Failure(e) => {
                VariableParseError::from_error_kind(e.input, e.code)
            }
            nom::Err::Incomplete(e) => nom::Err::Incomplete(e).into(),
        }
    }
}

pub(crate) fn variable_reference<N: FromStr + ToString>(
    input: Span,
) -> IResult<Span, VariableReference<N>, VariableParseError<Span>> {
    map(
        tuple((namespace, many0(preceded(char('.'), path_part)))),
        |(namespace, path)| {
            let location = namespace.location.start
                ..path
                    .last()
                    .map(|p| p.location.end)
                    .unwrap_or(namespace.location.end);
            VariableReference {
                namespace,
                path,
                location,
            }
        },
    )(input)
}

fn path_part(input: Span) -> IResult<Span, VariablePathPart, VariableParseError<Span>> {
    map(identifier, |span| VariablePathPart {
        part: Cow::from(*span.fragment()),
        location: span.location_offset()..span.location_offset() + span.fragment().len(),
    })(input)
    .map_err(|e| nom::Err::Error(e.into()))
}

fn namespace<N: FromStr + ToString>(
    input: Span,
) -> IResult<Span, VariableNamespace<N>, VariableParseError<Span>> {
    match recognize(pair(char('$'), identifier))(input) {
        Ok((remaining, span)) => match span.fragment().parse::<N>() {
            Ok(namespace) => Ok((
                remaining,
                VariableNamespace {
                    namespace,
                    location: span.location_offset()
                        ..span.location_offset() + span.fragment().len(),
                },
            )),
            Err(_) => Err(nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: span.fragment().to_string(),
                location: span.location_offset()..span.location_offset() + span.fragment().len(),
            })),
        },
        Err(_) => {
            let end = input
                .fragment()
                .find(['.', '}'])
                .unwrap_or(input.fragment().len());
            Err(nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: input.fragment()[0..end].to_string(),
                location: input.location_offset()..input.location_offset() + end,
            }))
        }
    }
}

fn identifier(input: Span) -> IResult<Span, Span> {
    recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ))(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::connect::variable::Namespace;

    #[test]
    fn test_namespace() {
        let result = namespace::<Namespace>(Span::new("$args")).unwrap().1;
        assert_eq!(result.namespace, Namespace::Args);
        assert_eq!(result.location, 0..5);

        let result = namespace::<Namespace>(Span::new("$status")).unwrap().1;
        assert_eq!(result.namespace, Namespace::Status);
        assert_eq!(result.location, 0..7);

        assert_eq!(
            namespace::<Namespace>(Span::new("$foobar")).unwrap_err(),
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "$foobar".into(),
                location: 0..7,
            })
        );

        assert_eq!(
            namespace::<Namespace>(Span::new("foobar")).unwrap_err(),
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "foobar".into(),
                location: 0..6,
            })
        );

        assert_eq!(
            namespace::<Namespace>(Span::new("")).unwrap_err(),
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "".into(),
                location: 0..0,
            })
        );
    }

    #[test]
    fn test_variable_reference() {
        let result = variable_reference::<Namespace>(Span::new("$this.a.b.c"))
            .unwrap()
            .1;
        assert_eq!(result.namespace.namespace, Namespace::This);
        assert_eq!(result.path.len(), 3);
        assert_eq!(result.path[0].part, "a");
        assert_eq!(result.path[1].part, "b");
        assert_eq!(result.path[2].part, "c");
        assert_eq!(result.location, 0..11);
        assert_eq!(result.namespace.location, 0..5);
        assert_eq!(result.path[0].location, 6..7);
        assert_eq!(result.path[1].location, 8..9);
        assert_eq!(result.path[2].location, 10..11);
    }

    #[test]
    fn test_invalid_namespace() {
        let result = variable_reference::<Namespace>(Span::new("$foo.a.b.c")).unwrap_err();
        assert_eq!(
            result,
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "$foo".into(),
                location: 0..4,
            })
        )
    }

    #[test]
    fn test_namespace_missing_dollar() {
        assert_eq!(
            variable_reference::<Namespace>(Span::new("foo.a.b.c} After")).unwrap_err(),
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "foo".into(),
                location: 0..3,
            })
        );
        assert_eq!(
            variable_reference::<Namespace>(Span::new("foo} After")).unwrap_err(),
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "foo".into(),
                location: 0..3,
            })
        );
    }
}
