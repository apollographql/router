use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Name;
use apollo_compiler::Node;

use super::DirectiveName;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_ENTITY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_NAME_ARGUMENT_NAME;

/// The location of a field within an object.
#[derive(Clone, Copy)]
pub(super) struct FieldCoordinate<'a> {
    pub object: &'a Node<ObjectType>,
    pub field: &'a Component<FieldDefinition>,
}

impl Display for FieldCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self { object, field } = self;
        write!(
            f,
            "`{object}.{field}`",
            object = object.name,
            field = field.name
        )
    }
}

/// The location of a `@connect` directive.
#[derive(Clone, Copy)]
pub(super) struct ConnectDirectiveCoordinate<'a> {
    pub connect_directive_name: &'a Name,
    pub field_coordinate: FieldCoordinate<'a>,
}

impl Display for ConnectDirectiveCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self {
            connect_directive_name,
            field_coordinate,
        } = self;
        write!(f, "`@{connect_directive_name}` on {field_coordinate}",)
    }
}

/// The coordinate of an `HTTP` arg within a connect directive.
pub(super) struct ConnectHTTPCoordinate<'a> {
    pub(crate) connect_directive_coordinate: ConnectDirectiveCoordinate<'a>,
}

impl Display for ConnectHTTPCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let ConnectDirectiveCoordinate {
            connect_directive_name,
            field_coordinate,
        } = self.connect_directive_coordinate;
        write!(
            f,
            "`@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on {field_coordinate}",
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
    pub(crate) http_method: &'a Name,
    pub(crate) node: &'a Node<Value>,
}

impl Display for HttpMethodCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self {
            connect:
                ConnectDirectiveCoordinate {
                    connect_directive_name,
                    field_coordinate,
                },
            http_method,
            node: _node,
        } = self;
        write!(
            f,
            "`{http_method}` in `@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on {field_coordinate}",
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

pub(super) fn connect_directive_selection_coordinate(
    connect_directive_name: &Name,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!("`@{connect_directive_name}({CONNECT_SELECTION_ARGUMENT_NAME}:)` on `{object_name}.{field}`", object_name = object.name)
}

pub(super) fn connect_directive_http_body_coordinate(
    connect_directive_name: &Name,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!("`@{connect_directive_name}({HTTP_ARGUMENT_NAME}: {{{CONNECT_BODY_ARGUMENT_NAME}:}})` on `{object_name}.{field}`", object_name = object.name)
}

pub(super) fn directive_http_header_coordinate(
    directive_name: &Name,
    argument_name: &str,
    object: Option<&Name>,
    field: Option<&Name>,
) -> String {
    match (object, field) {
        (Some(object), Some(field)) => {
            format!(
                "`@{directive_name}({argument_name}:)` on `{}.{}`",
                object, field
            )
        }
        _ => {
            format!("`@{directive_name}({argument_name}:)`")
        }
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
    object_name: &Name,
    field_name: &Name,
) -> String {
    format!("`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: {source})` on `{object_name}.{field_name}`")
}

/// Coordinate for an `HTTP.headers` argument in `@source` or `@connect`.
#[derive(Clone, Copy)]
pub(super) enum HttpHeadersCoordinate<'a> {
    Source {
        directive_name: &'a Name,
    },
    Connect {
        directive_name: &'a Name,
        object: &'a Name,
        field: &'a Name,
    },
}

impl Display for HttpHeadersCoordinate<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Connect {
                directive_name,
                object,
                field,
            } => {
                write!(
                    f,
                    "`@{directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)` on `{}.{}`",
                    object, field
                )
            }
            Self::Source { directive_name } => {
                write!(
                    f,
                    "`@{directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)`"
                )
            }
        }
    }
}

pub(super) fn connect_directive_entity_argument_coordinate(
    connect_directive_entity_argument: &Name,
    value: &Value,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!("`@{connect_directive_entity_argument}({CONNECT_ENTITY_ARGUMENT_NAME}: {value})` on `{object_name}.{field}`", object_name = object.name)
}

pub(super) fn field_with_connect_directive_entity_true_coordinate(
    connect_directive_entity_argument: &Name,
    value: &Value,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!("`{object_name}.{field}` with `@{connect_directive_entity_argument}({CONNECT_ENTITY_ARGUMENT_NAME}: {value})`", object_name = object.name)
}
