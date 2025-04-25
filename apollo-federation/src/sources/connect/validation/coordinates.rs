use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;

use super::DirectiveName;
use crate::sources::connect::HTTPMethod;
use crate::sources::connect::id::ConnectedElement;
use crate::sources::connect::spec::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;

/// The location of a `@connect` directive.
#[derive(Clone, Copy)]
pub(super) struct ConnectDirectiveCoordinate<'a> {
    pub(super) directive: &'a Node<Directive>,
    pub(super) element: ConnectedElement<'a>,
}

impl Display for ConnectDirectiveCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self { directive, element } = self;
        write!(
            f,
            "`@{connect_directive_name}` on `{element}`",
            connect_directive_name = directive.name
        )
    }
}

/// The location of a `@source` directive.
#[derive(Clone, Copy)]
pub(super) struct SourceDirectiveCoordinate<'a> {
    pub(super) directive: &'a Node<Directive>,
}

impl Display for SourceDirectiveCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self { directive } = self;
        write!(
            f,
            "`@{connect_directive_name}`",
            connect_directive_name = directive.name
        )
    }
}

#[derive(Clone, Copy)]
pub(super) struct SelectionCoordinate<'a> {
    pub(crate) connect: ConnectDirectiveCoordinate<'a>,
}

impl Display for SelectionCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let ConnectDirectiveCoordinate { directive, element } = self.connect;
        write!(
            f,
            "`@{connect_directive_name}({CONNECT_SELECTION_ARGUMENT_NAME}:)` on `{element}`",
            connect_directive_name = directive.name
        )
    }
}

impl<'a> From<ConnectDirectiveCoordinate<'a>> for SelectionCoordinate<'a> {
    fn from(connect_directive_coordinate: ConnectDirectiveCoordinate<'a>) -> Self {
        Self {
            connect: connect_directive_coordinate,
        }
    }
}

/// The coordinate of an `HTTP` arg within a connect directive.
pub(super) struct ConnectHTTPCoordinate<'a> {
    pub(crate) connect_directive_coordinate: ConnectDirectiveCoordinate<'a>,
}

impl Display for ConnectHTTPCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let ConnectDirectiveCoordinate { directive, element } = self.connect_directive_coordinate;
        write!(
            f,
            "`@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on `{element}`",
            connect_directive_name = directive.name
        )
    }
}

impl<'a> From<ConnectDirectiveCoordinate<'a>> for ConnectHTTPCoordinate<'a> {
    fn from(connect_directive_coordinate: ConnectDirectiveCoordinate<'a>) -> Self {
        Self {
            connect_directive_coordinate,
        }
    }
}

/// The coordinate of an `HTTP.method` arg within the `@connect` directive.
#[derive(Clone, Copy)]
pub(super) struct HttpMethodCoordinate<'a> {
    pub(crate) connect: ConnectDirectiveCoordinate<'a>,
    pub(crate) method: HTTPMethod,
    pub(crate) node: &'a Node<Value>,
}

impl Display for HttpMethodCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self {
            connect: ConnectDirectiveCoordinate { directive, element },
            method,
            node: _node,
        } = self;
        write!(
            f,
            "`{method}` in `@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on `{element}`",
            connect_directive_name = directive.name,
        )
    }
}

/// The `baseURL` argument for the `@source` directive
#[derive(Clone, Copy)]
pub(super) struct BaseUrlCoordinate<'a> {
    pub(crate) source_directive_name: &'a DirectiveName,
}

impl Display for BaseUrlCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self {
            source_directive_name,
        } = self;
        write!(
            f,
            "`@{source_directive_name}({SOURCE_BASE_URL_ARGUMENT_NAME}:)`",
        )
    }
}

pub(super) fn source_http_argument_coordinate(source_directive_name: &DirectiveName) -> String {
    format!("`@{source_directive_name}({HTTP_ARGUMENT_NAME}:)`")
}

pub(super) fn source_name_argument_coordinate(source_directive_name: &DirectiveName) -> String {
    format!("`@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}:)`")
}

pub(super) fn source_name_value_coordinate(
    source_directive_name: &DirectiveName,
    value: &Node<Value>,
) -> String {
    format!("`@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}: {value})`")
}

pub(super) fn connect_directive_name_coordinate(
    connect_directive_name: &Name,
    source: &Node<Value>,
    coordinate: &ConnectDirectiveCoordinate,
) -> String {
    format!(
        "`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: {source})` on `{element}`",
        element = coordinate.element
    )
}

/// Coordinate for an `HTTP.headers` argument in `@source` or `@connect`.
#[derive(Clone, Copy)]
pub(super) enum HttpHeadersCoordinate<'a> {
    Source {
        directive_name: &'a Name,
    },
    Connect {
        connect: ConnectDirectiveCoordinate<'a>,
    },
}

impl Display for HttpHeadersCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Connect {
                connect: ConnectDirectiveCoordinate { directive, element },
            } => {
                write!(
                    f,
                    "`@{connect_directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)` on `{element}`",
                    connect_directive_name = directive.name
                )
            }
            Self::Source { directive_name } => {
                write!(
                    f,
                    "`@{directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)`",
                )
            }
        }
    }
}
