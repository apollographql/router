use std::collections::HashMap;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Name;
use apollo_compiler::Node;
use http::HeaderName;

use crate::sources::connect::header::HeaderValue;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME as FROM_ARG;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME as NAME_ARG;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME as VALUE_ARG;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::require_value_is_str;
use crate::sources::connect::validation::variable::VariableResolver;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::variable::ConnectorsContext;
use crate::sources::connect::variable::Directive;
use crate::sources::connect::variable::Target;

pub(crate) fn validate_arg<'a>(
    http_arg: &'a [(Name, Node<Value>)],
    schema: &'a SchemaInfo,
    source_map: &'a SourceMap,
    coordinate: HttpHeadersCoordinate<'a>,
) -> Option<impl Iterator<Item = Message> + 'a> {
    let headers = get_arg(http_arg)?;
    #[allow(clippy::mutable_key_type)]
    let mut names = HashMap::new();
    let messages = headers
        .as_list()
        .into_iter()
        .flat_map(|l| l.iter().filter_map(|o| o.as_object()))
        .chain(headers.as_object())
        .flat_map(move |arg_pairs| {
            let mut messages = Vec::new();

            let name_arg = arg_pairs.iter().find_map(|(key, value)| (key == &NAME_ARG).then_some(value));
            let value_arg = arg_pairs.iter().find_map(|(key, value)| (key == &VALUE_ARG).then_some(value));
            let from_arg = arg_pairs.iter().find_map(|(key, value)| (key == &FROM_ARG).then_some(value));

            // validate `name`
            let Some(name_value) = name_arg else {
                // `name` must be provided
                messages.push(Message {
                    code: Code::GraphQLError,
                    message: format!("{coordinate} must include a `name` value."),
                    // TODO: get this closer to the pair
                    locations: headers.line_column_range(source_map)
                        .into_iter()
                        .collect(),
                });
                return messages;
            };

            let header_name = match validate_name(&NAME_ARG, name_value, coordinate, source_map) {
                Err(err) => {
                    messages.push(err);
                    None
                },
                Ok(value) => Some(value),
            };

            if let Some(header_name) = header_name {
                if let Some(duplicate) = names.insert(header_name.clone(), name_value.location()) {
                    messages.push(Message {
                        code: Code::HttpHeaderNameCollision,
                        message: format!(
                            "Duplicate header names are not allowed. The header name '{header_name}' at {coordinate} is already defined.",
                        ),
                        locations: name_value.line_column_range(source_map)
                            .into_iter()
                            .chain(
                                duplicate.and_then(|span| span.line_column_range(source_map))
                            )
                            .collect(),
                    });
                }
            }

            messages.extend(match (from_arg, value_arg)  {
                (Some(from_value), None) => {
                    validate_name(&FROM_ARG, from_value, coordinate, source_map).err()
                },
                (None, Some(value_arg)) => {
                    validate_value(value_arg, schema, coordinate, source_map)
                },
                (Some(from_arg), Some(value_arg)) => Some(
                    Message {
                        code: Code::InvalidHttpHeaderMapping,
                        message: format!("{coordinate} uses both `from` and `value` keys together. Please choose only one."),
                        locations: from_arg.line_column_range(source_map)
                            .into_iter()
                            .chain(
                                value_arg.line_column_range(source_map)
                            )
                            .collect(),
                    }
                ),
                (None, None) => Some(
                    Message {
                        code: Code::MissingHeaderSource,
                        message: format!("{coordinate} must include either a `from` or `value` argument."),
                        locations: headers.line_column_range(source_map)
                            .into_iter()
                            .collect(),
                    }
                )
            });

            messages
        });
    Some(messages)
}

fn validate_value(
    value: &Node<Value>,
    schema: &SchemaInfo,
    coordinate: HttpHeadersCoordinate,
    source_map: &SourceMap,
) -> Option<Message> {
    let str_value = match require_value_is_str(value, coordinate, source_map) {
        Ok(str_value) => str_value,
        Err(err) => return Some(err),
    };

    if let Err(message) = http::HeaderValue::try_from(str_value).map_err(|_| Message {
        code: Code::InvalidHttpHeaderValue,
        message: format!("The value `{str_value}` at {coordinate} is an invalid HTTP header"),
        locations: value.line_column_range(source_map).into_iter().collect(),
    }) {
        return Some(message);
    }

    let variable_resolver = match coordinate {
        HttpHeadersCoordinate::Source { .. } => VariableResolver::new(
            ConnectorsContext::new(Directive::Source, Target::RequestHeader),
            schema,
        ),
        HttpHeadersCoordinate::Connect { connect, .. } => VariableResolver::new(
            ConnectorsContext::new(connect.into(), Target::RequestHeader),
            schema,
        ),
    };

    // Validate that the header value can be parsed, then validate any variable expressions
    match HeaderValue::from_str(str_value).map_err(|_| Message {
        code: Code::InvalidHttpHeaderValue,
        message: format!("The value `{str_value}` at {coordinate} is an invalid HTTP header"),
        locations: value.line_column_range(source_map).into_iter().collect(),
    }) {
        Err(message) => return Some(message),
        Ok(header_value) => {
            let expression = GraphQLString::new(value, &schema.sources)
                .map_err(|_| Message {
                    code: Code::GraphQLError,
                    message: format!("The value for {coordinate} must be a string."),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                })
                .ok()?;
            for reference in header_value.variable_references() {
                match variable_resolver.resolve(reference, expression) {
                    Err(message) => {
                        return Some(Message {
                            code: message.code,
                            message: format!("{coordinate} contains an invalid variable reference {{{reference}}} - {message}", message = message.message),
                            locations: message.locations,
                        })
                    },
                    Ok(Some(ty)) => {
                        if !ty.is_non_null() {
                            return Some(Message {
                                code: Code::NullablePathVariable,
                                message: format!(
                                    "Variables in headers should be non-null, but {coordinate} contains `{{{reference}}}` which is nullable. \
                                    If a null value is provided at runtime, the header will be incorrect.",
                                ),
                                locations: expression
                                    .line_col_for_subslice(reference.location.clone(), schema)
                                    .into_iter()
                                    .collect(),
                            });
                        }
                    }
                    Ok(_) => {} // Type cannot be resolved
                }
            }
        }
    };

    None
}

fn validate_name(
    key: &Name,
    value: &Node<Value>,
    coordinate: HttpHeadersCoordinate,
    source_map: &SourceMap,
) -> Result<HeaderName, Message> {
    let s = value.as_str().ok_or_else(|| Message {
        code: Code::GraphQLError,
        message: format!("{coordinate} contains an invalid header name type."),
        locations: value.line_column_range(source_map).into_iter().collect(),
    })?;

    HeaderName::try_from(s).map_err(|_| Message {
        code: Code::InvalidHttpHeaderName,
        message: format!(
            "The value `{}` for `{}` at {} must be a valid HTTP header name.",
            s, key, coordinate
        ),
        locations: value.line_column_range(source_map).into_iter().collect(),
    })
}

fn get_arg(http_arg: &[(Name, Node<Value>)]) -> Option<&Node<Value>> {
    http_arg
        .iter()
        .find_map(|(key, value)| (*key == HEADERS_ARGUMENT_NAME).then_some(value))
}
