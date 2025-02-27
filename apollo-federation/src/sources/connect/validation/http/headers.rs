use std::collections::HashMap;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;

use crate::sources::connect::HeaderSource;
use crate::sources::connect::models::Header;
use crate::sources::connect::models::HeaderParseError;
use crate::sources::connect::spec::schema::HEADERS_ARGUMENT_NAME;
use crate::sources::connect::string_template;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::scalars;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;

pub(crate) fn validate_arg<'a>(
    http_arg: &'a [(Name, Node<Value>)],
    coordinate: HttpHeadersCoordinate<'a>,
    schema: &SchemaInfo,
) -> Vec<Message> {
    let sources = &schema.sources;
    let mut messages = Vec::new();
    let Some(headers_arg) = get_arg(http_arg) else {
        return messages;
    };

    #[allow(clippy::mutable_key_type)]
    let mut names = HashMap::new();
    for header in Header::from_headers_arg(headers_arg) {
        let Header {
            name,
            name_node,
            source,
            source_node,
        } = match header {
            Ok(header) => header,
            Err(err) => {
                let (message, locations) = match err {
                    HeaderParseError::Other { message, node } => (
                        message,
                        node.line_column_range(sources).into_iter().collect(),
                    ),
                    HeaderParseError::ConflictingArguments {
                        message,
                        from_location,
                        value_location,
                    } => (
                        message,
                        from_location
                            .iter()
                            .chain(value_location.iter())
                            .flat_map(|span| span.line_column_range(sources))
                            .collect(),
                    ),
                    HeaderParseError::ValueError {
                        err: string_template::Error { message, location },
                        node,
                    } => (
                        message,
                        GraphQLString::new(node, sources)
                            .ok()
                            .and_then(|expression| {
                                expression.line_col_for_subslice(location, schema)
                            })
                            .into_iter()
                            .collect(),
                    ),
                };
                messages.push(Message {
                    code: Code::InvalidHeader,
                    message: format!("In {coordinate} {message}"),
                    locations,
                });
                continue;
            }
        };
        if let Some(duplicate) = names.insert(name.clone(), name_node.location()) {
            messages.push(Message {
                code: Code::HttpHeaderNameCollision,
                message: format!(
                    "Duplicate header names are not allowed. The header name '{name}' at {coordinate} is already defined.",
                ),
                locations: name_node.line_column_range(sources)
                    .into_iter()
                    .chain(
                        duplicate.and_then(|span| span.line_column_range(sources))
                    )
                    .collect(),
            });
            continue;
        }
        if let HeaderSource::Value(header_value) = source {
            let Ok(expression) = GraphQLString::new(source_node, sources) else {
                // This should never fail in practice, we convert to GraphQLString only to hack in location data
                continue;
            };
            let expression_context = match coordinate {
                HttpHeadersCoordinate::Source { .. } => {
                    expression::Context::for_source(schema, &expression, Code::InvalidHeader)
                }
                HttpHeadersCoordinate::Connect { connect, .. } => {
                    expression::Context::for_connect_request(
                        schema,
                        connect,
                        &expression,
                        Code::InvalidHeader,
                    )
                }
            };
            messages.extend(
                header_value
                    .expressions()
                    .filter_map(|expression| {
                        expression::validate(expression, &expression_context, &scalars()).err()
                    })
                    .map(|mut err| {
                        err.message = format!("In {coordinate}: {}", err.message);
                        err
                    }),
            );
        }
    }
    messages
}

fn get_arg(http_arg: &[(Name, Node<Value>)]) -> Option<&Node<Value>> {
    http_arg
        .iter()
        .find_map(|(key, value)| (*key == HEADERS_ARGUMENT_NAME).then_some(value))
}
