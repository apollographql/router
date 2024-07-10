use apollo_compiler::Node;
use apollo_compiler::SourceMap;

use super::parse_url;
use super::Code;
use super::Location;
use super::Message;
use super::Value;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;

pub(super) fn validate_http_url(
    http_arg_url: Option<(&Node<Value>, String)>,
    source_map: &SourceMap,
) -> Vec<Message> {
    let mut messages = vec![];

    if let Some((url, url_coordinate)) = http_arg_url {
        if parse_url(url, &url_coordinate, source_map).is_ok() {
            messages.push(Message {
                code: Code::AbsoluteConnectUrlWithSource,
                message: format!(
                    "{url_coordinate} contains the absolute URL {url} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to a path.",
                ),
                locations: Location::from_node(url.location(), source_map)
                    .into_iter()
                    .collect(),
            });
        }
    }

    messages
}
