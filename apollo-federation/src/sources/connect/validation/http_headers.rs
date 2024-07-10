use std::collections::HashSet;

use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::SourceMap;
use http::HeaderName;
use itertools::Itertools;

use super::coordinates::http_header_argument_coordinate;
use super::Code;
use super::Location;
use super::Message;

pub(super) fn validate_headers_arg(
    directive_name: &Name,
    argument_name: &str,
    headers: &Node<Value>,
    source_map: &SourceMap,
    object: Option<&Name>,
    field: Option<&Name>,
) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut unique_header_set = HashSet::new();

    headers
        .as_list()
        .map(|l| l.iter().filter_map(|o| o.as_object()).collect_vec())
        .unwrap_or_else(|| headers.as_object().map(|o| vec![o]).unwrap_or_default())
        .into_iter()
        .for_each(|arg_pairs| {
            let pair_coordinate = &http_header_argument_coordinate(
                directive_name,
                argument_name,
                object,
                field,
            );

            let name_arg = arg_pairs.iter().find_map(|(key, value)| (key == &name!("name")).then_some(value));
            let as_arg = arg_pairs.iter().find_map(|(key, value)| (key == &name!("as")).then_some(value));
            let value_arg = arg_pairs.iter().find_map(|(key, value)| (key == &name!("value")).then_some(value));
            let from_arg = arg_pairs.iter().find_map(|(key, value)| (key == &name!("from")).then_some(value));

            // validate `name`
            if let Some(name_value) = name_arg {
                if let Some(err) = validate_header_name(&name!("name"), name_value, pair_coordinate, source_map).err() {
                    messages.push(err);
                } else if let Some(s) = name_value.as_str() {
                    if !unique_header_set.insert(s) {
                        messages.push(Message {
                            code: Code::HttpHeaderNameCollision,
                            message: format!("{pair_coordinate} must have a unique value for `name`."),
                            locations: Location::from_node(name_value.location(), source_map)
                                .into_iter()
                                .collect(),
                        });
                    }
                }
            } else {
                // `name` must be provided
                messages.push(Message {
                    code: Code::HttpHeaderNameCollision,
                    message: format!("{pair_coordinate} must include a `name` value."),
                    // TODO: get this closer to the pair
                    locations: Location::from_node(headers.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            }

            // validate `from`
            if let Some(from_value) = from_arg {
                if let Some(err) = validate_header_name(&name!("from"), from_value, pair_coordinate, source_map).err() {
                    messages.push(err);
                }
            }

            if let (Some(from_arg), Some(name_arg)) = (from_arg, name_arg) {
                if let (Some(from_value), Some(name_value)) = (from_arg.as_str(), name_arg.as_str()) {
                    if from_value == name_value {
                        messages.push(Message {
                            code: Code::HttpHeaderNameCollision,
                            message: format!("{pair_coordinate} must have unique values for `name` and `from` keys."),
                            locations: Location::from_node(from_arg.location(), source_map)
                                .into_iter()
                                .collect(),
                        });
                    }
                }
            }

            // validate `as`
            if let Some(as_value) = as_arg {
                if let Some(err) = validate_header_name(&name!("as"), as_value, pair_coordinate, source_map).err() {
                    messages.push(err);
                }
            }

            if let (Some(as_arg), Some(name_arg)) = (as_arg, name_arg) {
                if let (Some(as_value), Some(name_value)) = (as_arg.as_str(), name_arg.as_str()) {
                    if as_value == name_value {
                        messages.push(Message {
                            code: Code::HttpHeaderNameCollision,
                            message: format!("{pair_coordinate} must have unique values for `name` and `as` keys."),
                            locations: Location::from_node(as_arg.location(), source_map)
                                .into_iter()
                                .collect(),
                        });
                    }
                }
            }

            // validate `value`
            if let Some(value_arg) = value_arg {
                let header_value_errors = validate_header_value(value_arg, pair_coordinate, source_map);
                if !header_value_errors.is_empty() {
                    messages.extend(header_value_errors);
                }
            }

            // `as` and `value` cannot be used together
            if let (Some(as_arg), Some(_value_arg)) = (as_arg, value_arg) {
                messages.push(Message {
                    code: Code::InvalidHttpHeaderMapping,
                    message: format!("{pair_coordinate} uses both `as` and `value` keys together. Please choose only one."),
                    locations: Location::from_node(as_arg.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            }

            // `from`` and `value` cannot be used together
            if let (Some(from_arg), Some(_value_arg)) = (from_arg, value_arg) {
                messages.push(Message {
                    code: Code::InvalidHttpHeaderMapping,
                    message: format!("{pair_coordinate} uses both `from` and `value` keys together. Please choose only one."),
                    locations: Location::from_node(from_arg.location(), source_map)
                        .into_iter()
                        .collect(),
                });
            }
        });

    messages
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

pub(super) fn get_http_headers_arg<'a>(
    http_arg: &'a [(Name, Node<Value>)],
    arg_name: &Name,
) -> Option<&'a Node<Value>> {
    http_arg
        .iter()
        .find_map(|(key, value)| (key == arg_name).then_some(value))
}
