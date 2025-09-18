use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use http::HeaderName;
use indexmap::IndexMap;

use crate::connectors::HeaderSource;
use crate::connectors::OriginatingDirective;
use crate::connectors::models::Header;
use crate::connectors::models::HeaderParseError;
use crate::connectors::string_template;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::coordinates::HttpHeadersCoordinate;
use crate::connectors::validation::expression;
use crate::connectors::validation::expression::scalars;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::graphql::subslice_location;

pub(crate) struct Headers<'schema> {
    headers: Vec<Header>,
    coordinate: HttpHeadersCoordinate<'schema>,
}

impl<'schema> Headers<'schema> {
    pub(crate) fn parse(
        http_arg: &'schema [(Name, Node<Value>)],
        coordinate: HttpHeadersCoordinate<'schema>,
        schema: &SchemaInfo,
    ) -> Result<Self, Vec<Message>> {
        let sources = &schema.sources;
        let mut messages = Vec::new();
        let originating_directive = match coordinate {
            HttpHeadersCoordinate::Source { .. } => OriginatingDirective::Source,
            HttpHeadersCoordinate::Connect { .. } => OriginatingDirective::Connect,
        };
        #[allow(clippy::mutable_key_type)]
        let mut headers: IndexMap<HeaderName, Header> = IndexMap::new();
        let connect_spec = schema.connect_link.spec;
        for header in Header::from_http_arg(http_arg, originating_directive, connect_spec) {
            let header = match header {
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
                            subslice_location(&node, location, schema)
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
            if let Some(duplicate) = headers.get(&header.name) {
                messages.push(Message {
                    code: Code::HttpHeaderNameCollision,
                    message: format!(
                        "Duplicate header names are not allowed. The header name '{name}' at {coordinate} is already defined.",
                        name = header.name
                    ),
                    locations: header.name_node.as_ref().and_then(|name| name.line_column_range(sources))
                        .into_iter()
                        .chain(
                            duplicate.name_node.as_ref().and_then(|name| name.line_column_range(sources))
                        )
                        .collect(),
                });
                continue;
            }
            headers.insert(header.name.clone(), header);
        }
        if messages.is_empty() {
            Ok(Self {
                headers: headers.into_values().collect(),
                coordinate,
            })
        } else {
            Err(messages)
        }
    }

    // TODO: return extracted keys here?
    pub(crate) fn type_check(self, schema: &SchemaInfo) -> Result<(), Vec<Message>> {
        let coordinate = self.coordinate;
        let mut messages = Vec::new();
        for header in self.headers {
            let HeaderSource::Value(header_value) = &header.source else {
                continue;
            };
            let Some(node) = header.source_node.as_ref() else {
                continue;
            };
            let expression_context = match coordinate {
                HttpHeadersCoordinate::Source { .. } => {
                    expression::Context::for_source(schema, node, Code::InvalidHeader)
                }
                HttpHeadersCoordinate::Connect { connect, .. } => {
                    expression::Context::for_connect_request(
                        schema,
                        connect,
                        node,
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
            )
        }
        if messages.is_empty() {
            Ok(())
        } else {
            Err(messages)
        }
    }
}
