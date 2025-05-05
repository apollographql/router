//! Parsing and validation for `@connect(http:)`

pub(crate) mod url_properties;

use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use http::Uri;
use multi_try::MultiTry;
use shape::Shape;
use url_properties::UrlProperties;

use crate::sources::connect::HTTPMethod;
use crate::sources::connect::JSONSelection;
use crate::sources::connect::Namespace;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::string_template;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::string_template::Part;
use crate::sources::connect::string_template::StringTemplate;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::coordinates::ConnectHTTPCoordinate;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::Context;
use crate::sources::connect::validation::expression::scalars;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::http::headers::Headers;
use crate::sources::connect::validation::http::url::validate_url_scheme;
use crate::sources::connect::validation::source::SourceName;

/// A valid, parsed (but not type-checked) `@connect(http:)`.
///
/// TODO: Use this when creating a `HttpJsonTransport` as well.
pub(super) struct Http<'schema> {
    transport: Transport<'schema>,
    body: Option<Body<'schema>>,
    headers: Headers<'schema>,
}

impl<'schema> Http<'schema> {
    /// Parse the `@connect(http:)` argument and run just enough checks to be able to use the
    /// argument at runtime. More advanced checks are done in [`Self::type_check`].
    ///
    /// Three sub-pieces are always parsed, and the errors from _all_ of those pieces are returned
    /// together in the event of failure:
    /// 1. `http.body` with [`Body::parse`]
    /// 2. `http.headers` with [`Headers::parse`]
    /// 3. `http.<METHOD>` (for example, `http.GET`) with [`Transport::parse`]
    ///
    /// The order these pieces run in doesn't matter and shouldn't affect the output.
    pub(super) fn parse(
        coordinate: ConnectDirectiveCoordinate<'schema>,
        source_name: Option<&'schema SourceName>,
        schema: &'schema SchemaInfo,
    ) -> Result<Self, Vec<Message>> {
        let Some((http_arg, http_arg_node)) = coordinate
            .directive
            .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
            .and_then(|arg| Some((arg.as_object()?, arg)))
        else {
            return Err(vec![Message {
                code: Code::GraphQLError,
                message: format!("{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument."),
                locations: coordinate
                    .directive
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }]);
        };

        Body::parse(http_arg, coordinate, schema)
            .map_err(|err| vec![err])
            .and_try(Headers::parse(
                http_arg,
                HttpHeadersCoordinate::Connect {
                    connect: coordinate,
                },
                schema,
            ))
            .and_try(
                Transport::parse(
                    http_arg,
                    ConnectHTTPCoordinate::from(coordinate),
                    http_arg_node,
                    source_name,
                    schema,
                )
                .map_err(|err| vec![err]),
            )
            .map_err(|nested| nested.into_iter().flatten().collect())
            .map(|(body, headers, transport)| Self {
                body,
                headers,
                transport,
            })
    }

    /// Type-check the `@connect(http:)` directive.
    ///
    /// Does things like ensuring that every accessed variable actually exists and that expressions
    /// used in the URL and headers result in scalars.
    ///
    /// TODO: Return some type checking results, like extracted keys?
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Vec<Message>> {
        let Self {
            transport,
            body,
            headers,
        } = self;

        let mut errors = Vec::new();
        if let Some(body) = body {
            errors.extend(body.type_check(schema).err());
        }

        errors.extend(headers.type_check(schema).err().into_iter().flatten());
        errors.extend(transport.type_check(schema));

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub(super) fn variables(&self) -> impl Iterator<Item = Namespace> + '_ {
        self.transport
            .url
            .expressions()
            .flat_map(|e| {
                e.expression
                    .variable_references()
                    .map(|var_ref| var_ref.namespace.namespace)
            })
            .chain(self.body.as_ref().into_iter().flat_map(|b| {
                b.selection
                    .variable_references()
                    .map(|var_ref| var_ref.namespace.namespace)
            }))
    }
}

struct Body<'schema> {
    selection: JSONSelection,
    string: GraphQLString<'schema>,
    coordinate: BodyCoordinate<'schema>,
}

impl<'schema> Body<'schema> {
    pub(super) fn parse(
        http_arg: &'schema [(Name, Node<Value>)],
        connect: ConnectDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let Some((_, value)) = http_arg
            .iter()
            .find(|(name, _)| name == &CONNECT_BODY_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = BodyCoordinate { connect };

        // Ensure that the body selection is a valid JSON selection string
        let string = match GraphQLString::new(value, &schema.sources) {
            Ok(selection_str) => selection_str,
            Err(_) => {
                return Err(Message {
                    code: Code::GraphQLError,
                    message: format!("{coordinate} must be a string."),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        let selection = match JSONSelection::parse(string.as_str()) {
            Ok(selection) => selection,
            Err(err) => {
                return Err(Message {
                    code: Code::InvalidBody,
                    message: format!("{coordinate} is not valid: {err}"),
                    locations: value
                        .line_column_range(&schema.sources)
                        .into_iter()
                        .collect(),
                });
            }
        };
        if selection.is_empty() {
            return Err(Message {
                code: Code::InvalidBody,
                message: format!("{coordinate} is empty"),
                locations: value
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            });
        }

        Ok(Some(Self {
            selection,
            string,
            coordinate,
        }))
    }

    /// Check that the selection of the body matches the inputs at this location.
    ///
    /// TODO: check keys here?
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Self {
            selection,
            string,
            coordinate,
        } = self;
        expression::validate(
            &Expression {
                expression: selection,
                location: 0..string.as_str().len(),
            },
            &Context::for_connect_request(schema, coordinate.connect, &string, Code::InvalidBody),
            &Shape::unknown([]),
        )
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }
}

#[derive(Clone, Copy)]
struct BodyCoordinate<'schema> {
    connect: ConnectDirectiveCoordinate<'schema>,
}

impl Display for BodyCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({HTTP_ARGUMENT_NAME}: {{{CONNECT_BODY_ARGUMENT_NAME}:}})` on `{element}`",
            connect_directive_name = self.connect.directive.name,
            element = self.connect.element
        )
    }
}

/// The `@connect(http.<METHOD>:)` arg
struct Transport<'schema> {
    // TODO: once this is shared with `HttpJsonTransport`, this will be used
    #[allow(dead_code)]
    method: HTTPMethod,
    url: StringTemplate,
    url_string: GraphQLString<'schema>,
    coordinate: HttpMethodCoordinate<'schema>,

    url_properties: UrlProperties<'schema>,
}

impl<'schema> Transport<'schema> {
    fn parse(
        http_arg: &'schema [(Name, Node<Value>)],
        coordinate: ConnectHTTPCoordinate<'schema>,
        http_arg_node: &Node<Value>,
        source_name: Option<&SourceName<'schema>>,
        schema: &'schema SchemaInfo<'schema>,
    ) -> Result<Self, Message> {
        let source_map = &schema.sources;
        let mut methods = http_arg
            .iter()
            .filter_map(|(method, value)| {
                HTTPMethod::from_str(method)
                    .ok()
                    .map(|method| (method, value))
            })
            .peekable();

        let Some((method, method_value)) = methods.next() else {
            return Err(Message {
                code: Code::MissingHttpMethod,
                message: format!("{coordinate} must specify an HTTP method."),
                locations: http_arg_node
                    .line_column_range(source_map)
                    .into_iter()
                    .collect(),
            });
        };

        if methods.peek().is_some() {
            let locations = method_value
                .line_column_range(source_map)
                .into_iter()
                .chain(methods.filter_map(|(_, node)| node.line_column_range(source_map)))
                .collect();
            return Err(Message {
                code: Code::MultipleHttpMethods,
                message: format!("{coordinate} cannot specify more than one HTTP method."),
                locations,
            });
        }

        let url_properties = UrlProperties::parse_for_connector(coordinate.clone(), schema, http_arg)?;

        let coordinate = HttpMethodCoordinate {
            connect: coordinate.connect_directive_coordinate,
            method,
            node: method_value,
        };

        let url_string =
            GraphQLString::new(coordinate.node, &schema.sources).map_err(|_| Message {
                code: Code::GraphQLError,
                message: format!("The value for {coordinate} must be a string."),
                locations: coordinate
                    .node
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            })?;
        let url = StringTemplate::from_str(url_string.as_str()).map_err(
            |string_template::Error { message, location }| Message {
                code: Code::InvalidUrl,
                message: format!("In {coordinate}: {message}"),
                locations: url_string
                    .line_col_for_subslice(location, schema)
                    .into_iter()
                    .collect(),
            },
        )?;

        if source_name.is_some() {
            return if url_string.as_str().starts_with("http://")
                || url_string.as_str().starts_with("https://")
            {
                Err(Message {
                    code: Code::AbsoluteConnectUrlWithSource,
                    message: format!(
                        "{coordinate} contains the absolute URL {raw_value} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to be relative.",
                        raw_value = coordinate.node
                    ),
                    locations: coordinate
                        .node
                        .line_column_range(source_map)
                        .into_iter()
                        .collect(),
                })
            } else {
                Ok(Self {
                    method,
                    url,
                    url_string,
                    coordinate,
                    url_properties
                })
            };
        } else {
            validate_absolute_connect_url(&url, coordinate, coordinate.node, url_string, schema)?;
        }
        Ok(Self {
            method,
            url,
            url_string,
            coordinate,
            url_properties
        })
    }

    /// Type-check the `@connect(http:<METHOD>:)` directive.
    ///
    /// TODO: Return input shapes for keys instead reparsing for `Connector::resolvable_key` later
    fn type_check(self, schema: &SchemaInfo) -> Vec<Message> {
        let expression_context = Context::for_connect_request(
            schema,
            self.coordinate.connect,
            &self.url_string,
            Code::InvalidUrl,
        );

        let mut messages = Vec::new();
        for expression in self.url.expressions() {
            messages.extend(
                expression::validate(expression, &expression_context, &scalars())
                    .err()
                    .into_iter()
                    .map(|mut err| {
                        err.message = format!(
                            "In {coordinate}: {}",
                            err.message,
                            coordinate = self.coordinate
                        );
                        err
                    }),
            );
        }

        messages.extend(self.url_properties.type_check(schema));

        messages
    }
}

/// Additional validation rules when using `@connect` without `source:`
fn validate_absolute_connect_url(
    url: &StringTemplate,
    coordinate: HttpMethodCoordinate,
    value: &Node<Value>,
    str_value: GraphQLString,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let relative_url_error = || Message {
        code: Code::RelativeConnectUrlWithoutSource,
        message: format!(
            "{coordinate} specifies the relative URL {raw_value}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL including scheme (e.g. https://), or add a `@{source_directive_name}`.",
            raw_value = coordinate.node,
            source_directive_name = schema.source_directive_name(),
        ),
        locations: coordinate
            .node
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    };

    let Some(Part::Constant(first)) = url.parts.first() else {
        return Err(relative_url_error());
    };

    let Some((_scheme, without_scheme)) = first.value.split_once("://") else {
        return Err(relative_url_error());
    };

    if url.parts.len() > 1
        && !without_scheme.contains('/')
        && !without_scheme.contains('?')
        && !without_scheme.contains('#')
    {
        return Err(Message {
            code: Code::InvalidUrl,
            message: format!(
                "{coordinate} must not contain dynamic pieces in the domain section (before the first `/` or `?`).",
            ),
            locations: str_value
                .line_col_for_subslice(first.location.clone(), schema)
                .into_iter()
                .collect(),
        });
    }

    let base_url = Uri::from_str(first.value.trim()).map_err(|err| Message {
        code: Code::InvalidUrl,
        message: format!("In {coordinate}: {err}"),
        locations: str_value
            .line_col_for_subslice(first.location.clone(), schema)
            .into_iter()
            .collect(),
    })?;
    validate_url_scheme(&base_url, coordinate, value, str_value, schema)?;

    Ok(())
}
