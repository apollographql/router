use std::collections::HashMap;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Name;
use apollo_compiler::Node;

use crate::sources::connect::models::Header;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;

pub(crate) fn validate_arg<'a>(
    http_arg: &'a [(Name, Node<Value>)],
    source_map: &'a SourceMap,
    coordinate: HttpHeadersCoordinate<'a>,
) -> Vec<Message> {
    let mut messages = Vec::new();
    let Some(headers_arg) = get_arg(http_arg) else {
        return messages;
    };
    #[allow(clippy::mutable_key_type)]
    let mut names = HashMap::new();
    for header in Header::from_headers_arg(headers_arg) {
        let Header {
            name,
            source: _,
            node,
        } = match header {
            Ok(header) => header,
            Err((message, node)) => {
                messages.push(Message {
                    code: Code::InvalidHeader,
                    message: format!("In {coordinate} {message}"),
                    locations: node.line_column_range(source_map).into_iter().collect(),
                });
                continue;
            }
        };
        if let Some(duplicate) = names.insert(name.clone(), node.location()) {
            messages.push(Message {
                code: Code::HttpHeaderNameCollision,
                message: format!(
                    "Duplicate header names are not allowed. The header name '{name}' at {coordinate} is already defined.",
                ),
                locations: node.line_column_range(source_map)
                    .into_iter()
                    .chain(
                        duplicate.and_then(|span| span.line_column_range(source_map))
                    )
                    .collect(),
            });
        }
    }
    messages
}

fn get_arg(http_arg: &[(Name, Node<Value>)]) -> Option<&Node<Value>> {
    http_arg
        .iter()
        .find_map(|(key, value)| (*key == HEADERS_ARGUMENT_NAME).then_some(value))
}
