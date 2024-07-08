use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Value;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::SourceMap;

use super::source_name_value_coordinate;
use super::Code;
use super::Location;
use super::Message;
use super::SourceName;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;

pub(super) fn validate_source_name_arg(
    field_name: &Name,
    object_name: &Name,
    source_name: &Node<Argument>,
    source_map: &SourceMap,
    source_names: &[SourceName],
    source_directive_name: &Name,
    connect_directive_name: &Name,
) -> Vec<Message> {
    let mut messages = vec![];

    if source_names.iter().all(|name| name != &source_name.value) {
        // TODO: Pick a suggestion that's not just the first defined source
        let qualified_directive = connect_directive_name_coordinate(
            connect_directive_name,
            &source_name.value,
            object_name,
            field_name,
        );
        if let Some(first_source_name) = source_names.first() {
            messages.push(Message {
                    code: Code::SourceNameMismatch,
                    message: format!(
                        "{qualified_directive} does not match any defined sources. Did you mean {first_source_name}?",
                    ),
                    locations: Location::from_node(source_name.location(), source_map)
                        .into_iter()
                        .collect(),
                });
        } else {
            messages.push(Message {
                    code: Code::NoSourcesDefined,
                    message: format!(
                        "{qualified_directive} specifies a source, but none are defined. Try adding {coordinate} to the schema.",
                        coordinate = source_name_value_coordinate(source_directive_name, &source_name.value),
                    ),
                    locations: Location::from_node(source_name.location(), source_map)
                        .into_iter()
                        .collect(),
                });
        }
    }

    messages
}

fn connect_directive_name_coordinate(
    connect_directive_name: &Name,
    source: &Node<Value>,
    object_name: &Name,
    field_name: &Name,
) -> String {
    format!("`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: {source})` on `{object_name}.{field_name}`")
}
