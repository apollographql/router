use apollo_compiler::parser::SourceMap;
use apollo_compiler::Node;

use super::Code;
use super::Message;
use super::Name;
use super::Value;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME;

pub(super) fn validate_http_method_arg(
    http_methods: &[&(Name, Node<Value>)],
    connect_directive_http_coordinate: String,
    http_arg_node: &Node<Value>,
    source_map: &SourceMap,
) -> Vec<Message> {
    let mut messages = vec![];

    if http_methods.len() > 1 {
        messages.push(Message {
            code: Code::MultipleHttpMethods,
            message: format!(
                "{connect_directive_http_coordinate} cannot specify more than one HTTP method.",
            ),
            locations: http_methods
                .iter()
                .flat_map(|(_, node)| node.line_column_range(source_map).into_iter())
                .collect(),
        });
    } else if http_methods.is_empty() {
        messages.push(Message {
            code: Code::MissingHttpMethod,
            message: format!("{connect_directive_http_coordinate} must specify an HTTP method.",),
            locations: http_arg_node
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        });
    }

    messages
}

pub(super) fn get_http_methods_arg(http_arg: &[(Name, Node<Value>)]) -> Vec<&(Name, Node<Value>)> {
    http_arg
        .iter()
        .filter(|(method, _)| {
            [
                CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME,
            ]
            .contains(method)
        })
        .collect()
}
