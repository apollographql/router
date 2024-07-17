use apollo_compiler::ast::Value;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::SourceMap;
use http::HeaderName;
use itertools::Itertools;

use super::coordinates::http_header_argument_coordinate;
use super::Code;
use super::Location;
use super::Message;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME as FROM_ARG;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME as NAME_ARG;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME as VALUE_ARG;

pub(super) fn validate_headers_arg<'a>(
    directive_name: &'a Name,
    headers: &'a Node<Value>,
    source_map: &'a SourceMap,
    object: Option<&'a Name>,
    field: Option<&'a Name>,
) -> impl Iterator<Item = Message> + 'a {
    headers
        .as_list()
        .into_iter()
        .flat_map(|l| l.iter().filter_map(|o| o.as_object()))
        .chain(headers.as_object())
        .flat_map(move |arg_pairs| {
            let pair_coordinate = &http_header_argument_coordinate(
                directive_name,
                object,
                field,
            );
            let mut messages = Vec::new();

            let name_arg = arg_pairs.iter().find_map(|(key, value)| (key == &NAME_ARG).then_some(value));
            let value_arg = arg_pairs.iter().find_map(|(key, value)| (key == &VALUE_ARG).then_some(value));
            let from_arg = arg_pairs.iter().find_map(|(key, value)| (key == &FROM_ARG).then_some(value));

            // validate `name`
            let Some(name_value) = name_arg else {
                // `name` must be provided
                messages.push(Message {
                    code: Code::GraphQLError,
                    message: format!("{pair_coordinate} must include a `name` value."),
                    // TODO: get this closer to the pair
                    locations: Location::from_node(headers.location(), source_map)
                        .into_iter()
                        .collect(),
                });
                return messages;
            };

            if let Some(err) = validate_header_name(&NAME_ARG, name_value, pair_coordinate, source_map).err() {
                messages.push(err);
            }

            // validate `from`
            if let Some(from_value) = from_arg {
                if let Some(err) = validate_header_name(&FROM_ARG, from_value, pair_coordinate, source_map).err() {
                    messages.push(err);
                }
            }

            // validate `value`
            if let Some(value_arg) = value_arg {
                let header_value_errors = validate_header_value(value_arg, pair_coordinate, source_map);
                if !header_value_errors.is_empty() {
                    messages.extend(header_value_errors);
                }
            }

            // `from` and `value` cannot be used together
            if let (Some(from_arg), Some(value_arg)) = (from_arg, value_arg) {
                messages.push(Message {
                    code: Code::InvalidHttpHeaderMapping,
                    message: format!("{pair_coordinate} uses both `from` and `value` keys together. Please choose only one."),
                    locations: Location::from_node(from_arg.location(), source_map)
                        .into_iter()
                        .chain(
                            Location::from_node(value_arg.location(), source_map)
                        )
                        .collect(),
                });
            }
            messages
        })
}

pub(super) fn validate_header_value(
    value: &Node<Value>,
    coordinate: &String,
    source_map: &SourceMap,
) -> Vec<Message> {
    let mut messages = Vec::new();

    // Extract values from the node
    let values: Vec<&str> = if let Some(list) = value.as_list() {
        list.iter().filter_map(|v| v.as_str()).collect_vec()
    } else {
        value.as_str().map(|s| vec![s]).unwrap_or_default()
    };

    // Validate each value
    for v in &values {
        if http::HeaderValue::try_from(*v).is_err() {
            messages.push(Message {
                code: Code::InvalidHttpHeaderValue,
                message: format!(
                    "The value '{}' at '{}' must be a valid HTTP header value.",
                    v, coordinate
                ),
                locations: Location::from_node(value.location(), source_map)
                    .into_iter()
                    .collect(),
            });
        }
    }

    messages
}

fn validate_header_name<'a>(
    key: &Name,
    value: &'a Node<Value>,
    coordinate: &String,
    source_map: &SourceMap,
) -> Result<&'a str, Message> {
    let s = value.as_str().ok_or_else(|| Message {
        code: Code::GraphQLError,
        message: format!("{coordinate} contains an invalid header name type."),
        locations: Location::from_node(value.location(), source_map)
            .into_iter()
            .collect(),
    })?;

    HeaderName::try_from(s).map_err(|_| Message {
        code: Code::InvalidHttpHeaderName,
        message: format!(
            "The value '{}' for '{}' at '{}' must be a valid HTTP header name.",
            s, key, coordinate
        ),
        locations: Location::from_node(value.location(), source_map)
            .into_iter()
            .collect(),
    })?;

    Ok(s)
}

pub(super) fn get_http_headers_arg(http_arg: &[(Name, Node<Value>)]) -> Option<&Node<Value>> {
    http_arg
        .iter()
        .find_map(|(key, value)| (*key == HEADERS_ARGUMENT_NAME).then_some(value))
}
