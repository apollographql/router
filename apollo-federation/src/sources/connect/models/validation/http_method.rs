use apollo_compiler::Node;
use apollo_compiler::NodeLocation;
use apollo_compiler::SourceMap;

use super::Code;
use super::Location;
use super::Message;
use super::Name;
use super::Value;

pub(super) fn validate_http_method(
    http_methods: &[&(Name, Node<Value>)],
    connect_directive_http_coordinate: String,
    http_arg_location: Option<NodeLocation>,
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
                .flat_map(|(_, node)| Location::from_node(node.location(), source_map).into_iter())
                .collect(),
        });
    } else if http_methods.is_empty() {
        messages.push(Message {
            code: Code::MissingHttpMethod,
            message: format!("{connect_directive_http_coordinate} must specify an HTTP method.",),
            locations: Location::from_node(http_arg_location, source_map)
                .into_iter()
                .collect(),
        });
    }

    messages
}
