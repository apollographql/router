//! Variables used in connector directives `@connect` and `@source`.

use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
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
pub(crate) trait ExpressionContext<N> {
    /// Get the variable namespaces that are available in this context
    fn available_namespaces(&self) -> impl Iterator<Item = N>;
}

/// A variable context for Apollo Connectors. Variables are used within a `@connect` or `@source`
/// [`Directive`], and have a specific [`Target`].
pub(crate) struct ConnectorsContext<'schema> {
    pub(super) directive: Directive<'schema>,
    pub(super) target: Target,
}

impl<'schema> ConnectorsContext<'schema> {
    pub(crate) fn new(directive: Directive<'schema>, target: Target) -> Self {
        Self { directive, target }
    }
}

impl<'schema> ExpressionContext<Namespace> for ConnectorsContext<'schema> {
    fn available_namespaces(&self) -> impl Iterator<Item = Namespace> {
        match (&self.directive, &self.target) {
            (Directive::Source, Target::ResponseBody) => {
                vec![Namespace::Config, Namespace::Context, Namespace::Status]
            }
            (Directive::Source, _) => vec![Namespace::Config, Namespace::Context],
            (Directive::Connect { .. }, Target::ResponseBody) => {
                vec![
                    Namespace::Args,
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::Status,
                    Namespace::This,
                ]
            }
            (Directive::Connect { .. }, _) => {
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

/// The target of an expression containing a variable reference
#[allow(unused)]
pub(crate) enum Target {
    /// The expression is used in a request header
    RequestHeader,

    /// The expression is used in a request URL
    RequestUrl,

    /// The expression is used in a request body
    RequestBody,

    /// The expression is used in a response body
    ResponseBody,
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
    type Err = VariableError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "$args" => Ok(Self::Args),
            "$config" => Ok(Self::Config),
            "$context" => Ok(Self::Context),
            "$status" => Ok(Self::Status),
            "$this" => Ok(Self::This),
            _ => Err(VariableError {
                message: format!("Unknown variable namespace `{s}`"),
                location: Some(0..s.len()),
            }),
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
pub(crate) struct VariableReference<N: FromStr<Err = VariableError> + ToString> {
    /// The namespace of the variable - `$this`, `$args`, `$status`, etc.
    pub(crate) namespace: VariableNamespace<N>,

    /// The path elements of this reference. For example, the reference `$this.a.b.c`
    /// has path elements `a`, `b`, `c`. May be empty in some cases, as in the reference `$status`.
    pub(crate) path: Vec<VariablePathPart>,

    /// The location of the reference within the original text.
    pub(crate) location: Range<usize>,
}

impl<N: FromStr<Err = VariableError> + ToString> VariableReference<N> {
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
            .map_err(|e| VariableError {
                message: e.message,
                location: e
                    .location
                    .map(|range| range.start + start_offset..range.end + start_offset),
            })
    }
}

impl<N: FromStr<Err = VariableError> + ToString> FromStr for VariableReference<N> {
    type Err = VariableError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        variable_reference(Span::new(s))
            .map(|(_, reference)| reference)
            .map_err(move |e| match e {
                nom::Err::Error(VariableParseError::Nom(span, _)) => VariableError {
                    message: format!("Invalid variable reference `{s}`"),
                    location: Some(
                        span.location_offset()..span.location_offset() + span.fragment().len(),
                    ),
                },
                nom::Err::Error(VariableParseError::InvalidNamespace {
                    namespace,
                    location,
                }) => VariableError {
                    message: format!("Unknown variable namespace `{namespace}`"),
                    location: Some(location),
                },
                _ => VariableError {
                    message: format!("Invalid variable reference `{s}`"),
                    location: None,
                },
            })
    }
}
impl<N: FromStr<Err = VariableError> + ToString> Display for VariableReference<N> {
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
pub(crate) struct VariableNamespace<N: FromStr<Err = VariableError> + ToString> {
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

pub(crate) fn variable_reference<N: FromStr<Err = VariableError> + ToString>(
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

fn namespace<N: FromStr<Err = VariableError> + ToString>(
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
        Err(e) => Err(nom::Err::Error(e.into())),
    }
}

fn identifier(input: Span) -> IResult<Span, Span> {
    recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ))(input)
}

#[derive(Debug)]
pub(crate) struct VariableError {
    pub(crate) message: String,
    pub(crate) location: Option<Range<usize>>,
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

        let error = namespace::<Namespace>(Span::new("$foobar")).unwrap_err();
        assert_eq!(
            error,
            nom::Err::Error(VariableParseError::InvalidNamespace {
                namespace: "$foobar".into(),
                location: 0..7,
            })
        );

        let error = namespace::<Namespace>(Span::new("")).unwrap_err();
        assert_eq!(
            error,
            nom::Err::Error(VariableParseError::Nom(Span::new(""), ErrorKind::Char))
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
            ConnectorsContext::new(Directive::Source, Target::RequestUrl)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Target::RequestHeader)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Target::RequestBody)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Target::ResponseBody)
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
            type Err = VariableError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    "$Mordor" => Ok(MiddleEarth::Mordor),
                    "$Gondor" => Ok(MiddleEarth::Gondor),
                    "$Rohan" => Ok(MiddleEarth::Rohan),
                    _ => Err(VariableError {
                        message: format!("Unknown realm `{s}`"),
                        location: Some(0..s.len()),
                    }),
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
            "$Mordor.mount_doom.cracks_of_doom".parse().unwrap();
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
