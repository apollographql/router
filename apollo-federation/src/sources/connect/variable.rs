//! Variables used in connector directives `@connect` and `@source`.

use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use itertools::Itertools;
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

/// The context of an expression containing variable references. The context determines what
/// variable namespaces are available for use in the expression.
pub(crate) trait ExpressionContext<N: FromStr + ToString> {
    /// Get the variable namespaces that are available in this context
    fn available_namespaces(&self) -> impl Iterator<Item = N>;

    /// Get the list of namespaces joined as a comma separated list
    fn namespaces_joined(&self) -> String {
        self.available_namespaces()
            .map(|s| s.to_string())
            .sorted()
            .join(", ")
    }
}

/// A variable context for Apollo Connectors. Variables are used within a `@connect` or `@source`
/// [`Directive`], are used in a particular [`Phase`], and have a specific [`Target`].
#[derive(Clone, PartialEq)]
pub(crate) struct ConnectorsContext<'schema> {
    pub(super) directive: Directive<'schema>,
    pub(super) phase: Phase,
    pub(super) target: Target,
}

impl<'schema> ConnectorsContext<'schema> {
    pub(crate) fn new(directive: Directive<'schema>, phase: Phase, target: Target) -> Self {
        Self {
            directive,
            phase,
            target,
        }
    }
}

impl<'schema> ExpressionContext<Namespace> for ConnectorsContext<'schema> {
    fn available_namespaces(&self) -> impl Iterator<Item = Namespace> {
        match (&self.directive, &self.phase, &self.target) {
            (Directive::Source, Phase::Response, _) => {
                vec![Namespace::Config, Namespace::Context, Namespace::Status]
            }
            (Directive::Source, _, _) => vec![Namespace::Config, Namespace::Context],
            (Directive::Connect { .. }, Phase::Response, _) => {
                vec![
                    Namespace::Args,
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::Status,
                    Namespace::This,
                ]
            }
            (Directive::Connect { .. }, _, _) => {
                vec![
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::This,
                    Namespace::Args,
                ]
            }
        }
        .into_iter()
    }
}

/// An Apollo Connectors directive in a schema
#[derive(Clone, PartialEq)]
pub(crate) enum Directive<'schema> {
    /// A `@source` directive
    Source,

    /// A `@connect` directive
    Connect {
        /// The object type containing the field the directive is on
        object: &'schema Node<ObjectType>,

        /// The field definition of the field the directive is on
        field: &'schema Component<FieldDefinition>,
    },
}

/// The phase an expression is associated with
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Phase {
    /// The request phase
    Request,

    /// The response phase
    Response,
}

/// The target of an expression containing a variable reference
#[allow(unused)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
    /// The expression is used in an HTTP header
    Header,

    /// The expression is used in a URL
    Url,

    /// The expression is used in the body of a request or response
    Body,
}

/// The variable namespaces defined for Apollo Connectors
#[derive(PartialEq, Eq, Clone, Copy, Hash)]
pub(crate) enum Namespace {
    Args,
    Config,
    Context,
    Status,
    This,
}

impl Namespace {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Args => "$args",
            Self::Config => "$config",
            Self::Context => "$context",
            Self::Status => "$status",
            Self::This => "$this",
        }
    }
}

impl FromStr for Namespace {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "$args" => Ok(Self::Args),
            "$config" => Ok(Self::Config),
            "$context" => Ok(Self::Context),
            "$status" => Ok(Self::Status),
            "$this" => Ok(Self::This),
            _ => Err(()),
        }
    }
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Display for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

type Span<'a> = LocatedSpan<&'a str>;

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

/// A variable reference. Consists of a namespace starting with a `$` and an optional path
/// separated by '.' characters.
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariableReference<N: FromStr + ToString> {
    /// The namespace of the variable - `$this`, `$args`, `$status`, etc.
    pub(crate) namespace: VariableNamespace<N>,

    /// The path elements of this reference. For example, the reference `$this.a.b.c`
    /// has path elements `a`, `b`, `c`. May be empty in some cases, as in the reference `$status`.
    pub(crate) path: Vec<VariablePathPart>,

    /// The location of the reference within the original text.
    pub(crate) location: Range<usize>,
}

impl<N: FromStr + ToString> VariableReference<N> {
    /// Parse a variable reference at the given offset within the original text. The locations of
    /// the variable and the parts within it will be based on the provided offset.
    pub(crate) fn parse(reference: &str, start_offset: usize) -> Result<Self, VariableError> {
        VariableReference::from_str(reference)
            .map(|reference| Self {
                namespace: VariableNamespace {
                    namespace: reference.namespace.namespace,
                    location: reference.namespace.location.start + start_offset
                        ..reference.namespace.location.end + start_offset,
                },
                path: reference
                    .path
                    .into_iter()
                    .map(|path_part| VariablePathPart {
                        part: path_part.part,
                        location: path_part.location.start + start_offset
                            ..path_part.location.end + start_offset,
                    })
                    .collect(),
                location: reference.location.start + start_offset
                    ..reference.location.end + start_offset,
            })
            .map_err(|e| match e {
                VariableError::ParseError { message, location } => VariableError::ParseError {
                    message: message.to_string(),
                    location: location.start + start_offset..location.end + start_offset,
                },
                VariableError::InvalidNamespace {
                    namespace,
                    location,
                } => VariableError::InvalidNamespace {
                    namespace: namespace.to_string(),
                    location: location.start + start_offset..location.end + start_offset,
                },
            })
    }

    fn from_str(s: &str) -> Result<Self, VariableError> {
        variable_reference(Span::new(s))
            .map(|(_, reference)| reference)
            .map_err(move |e| match e {
                nom::Err::Error(e) => e.into(),
                _ => VariableError::ParseError {
                    message: format!("Invalid variable reference `{s}`"),
                    location: 0..s.len(),
                },
            })
    }
}

impl<N: FromStr + ToString> Display for VariableReference<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.namespace.namespace.to_string().as_str())?;
        for part in &self.path {
            f.write_str(".")?;
            f.write_str(part.as_str())?;
        }
        Ok(())
    }
}

/// A namespace in a variable reference, like `$this` in `$this.a.b.c`
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariableNamespace<N: FromStr + ToString> {
    pub(crate) namespace: N,
    pub(crate) location: Range<usize>,
}

/// Part of a variable path, like `a` in `$this.a.b.c`
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariablePathPart {
    pub(crate) part: String,
    pub(crate) location: Range<usize>,
}

impl VariablePathPart {
    pub(crate) fn as_str(&self) -> &str {
        self.part.as_str()
    }
}

impl Display for VariablePathPart {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.part.to_string().as_str())?;
        Ok(())
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
        part: span.fragment().to_string(),
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

#[derive(Debug)]
pub(crate) enum VariableError {
    InvalidNamespace {
        namespace: String,
        location: Range<usize>,
    },
    ParseError {
        message: String,
        location: Range<usize>,
    },
}

impl From<VariableParseError<Span<'_>>> for VariableError {
    fn from(value: VariableParseError<Span>) -> Self {
        match value {
            VariableParseError::Nom(span, _) => VariableError::ParseError {
                message: format!("Invalid variable reference `{s}`", s = span.fragment()),
                location: span.location_offset()..span.location_offset() + span.fragment().len(),
            },
            VariableParseError::InvalidNamespace {
                namespace,
                location,
            } => VariableError::InvalidNamespace {
                namespace,
                location,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_parse() {
        let result = VariableReference::<Namespace>::parse("$this.a.b.c", 20).unwrap();
        assert_eq!(result.namespace.namespace, Namespace::This);
        assert_eq!(result.path.len(), 3);
        assert_eq!(result.path[0].part, "a");
        assert_eq!(result.path[1].part, "b");
        assert_eq!(result.path[2].part, "c");
        assert_eq!(result.location, 20..31);
        assert_eq!(result.namespace.location, 20..25);
        assert_eq!(result.path[0].location, 26..27);
        assert_eq!(result.path[1].location, 28..29);
        assert_eq!(result.path[2].location, 30..31);
    }

    #[test]
    fn test_available_namespaces() {
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Url)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Header)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Body)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Response, Target::Header)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context, Namespace::Status,]
        );
    }

    #[test]
    fn test_generic_namespace() {
        #[derive(Debug, PartialEq)]
        enum MiddleEarth {
            Mordor,
            Gondor,
            Rohan,
        }

        impl FromStr for MiddleEarth {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    "$Mordor" => Ok(MiddleEarth::Mordor),
                    "$Gondor" => Ok(MiddleEarth::Gondor),
                    "$Rohan" => Ok(MiddleEarth::Rohan),
                    _ => Err(()),
                }
            }
        }

        impl Display for MiddleEarth {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    MiddleEarth::Mordor => f.write_str("$Mordor"),
                    MiddleEarth::Gondor => f.write_str("$Gondor"),
                    MiddleEarth::Rohan => f.write_str("$Rohan"),
                }
            }
        }

        let result: VariableReference<MiddleEarth> =
            VariableReference::parse("$Mordor.mount_doom.cracks_of_doom", 0).unwrap();
        assert_eq!(result.namespace.namespace, MiddleEarth::Mordor);
        assert_eq!(result.path.len(), 2);
        assert_eq!(result.path[0].part, "mount_doom");
        assert_eq!(result.path[1].part, "cracks_of_doom");
        assert_eq!(result.location, 0..33);
        assert_eq!(result.namespace.location, 0..7);
        assert_eq!(result.path[0].location, 8..18);
        assert_eq!(result.path[1].location, 19..33);
    }
}
