use apollo_compiler::ast::Value;
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

pub(super) fn connect_directive_coordinate(
    connect_directive_name: &Name,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!(
        "`@{connect_directive_name}` on `{object_name}.{field}`",
        object_name = object.name
    )
}

pub(super) fn connect_directive_http_coordinate(
    connect_directive_name: &Name,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!(
        "`@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on `{object_name}.{field}`",
        object_name = object.name
    )
}

pub(super) fn connect_directive_url_coordinate(
    connect_directive_name: &Name,
    http_method: &Name,
    object: &Node<ObjectType>,
    field: &Name,
) -> String {
    format!("`{http_method}` in `@{connect_directive_name}({HTTP_ARGUMENT_NAME}:)` on `{object_name}.{field}`", object_name = object.name)
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

pub(super) fn source_base_url_argument_coordinate(source_directive_name: &DirectiveName) -> String {
    format!("`@{source_directive_name}({SOURCE_BASE_URL_ARGUMENT_NAME}:)`")
}

pub(super) fn connect_directive_name_coordinate(
    connect_directive_name: &Name,
    source: &Node<Value>,
    object_name: &Name,
    field_name: &Name,
) -> String {
    format!("`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: {source})` on `{object_name}.{field_name}`")
}

pub(super) fn http_argument_coordinate(
    directive_name: &DirectiveName,
    argument_name: &Name,
) -> String {
    format!("`@{directive_name}({argument_name}:)`")
}

pub(super) fn http_header_argument_coordinate(
    directive_name: &Name,
    object: Option<&Name>,
    field: Option<&Name>,
) -> String {
    match (object, field) {
        (Some(object), Some(field)) => {
            format!(
                "`@{directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)` on `{}.{}`",
                object, field
            )
        }
        _ => {
            format!("`@{directive_name}({HTTP_ARGUMENT_NAME}.{HEADERS_ARGUMENT_NAME}:)`")
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
