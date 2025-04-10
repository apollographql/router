use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use itertools::Itertools;
use shape::Shape;

use crate::sources::connect::JSONSelection;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::GraphQLString;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::SchemaInfo;
use crate::sources::connect::validation::coordinates::ConnectHTTPCoordinate;
use crate::sources::connect::validation::expression;

pub(crate) struct Property<'schema> {
    expression: Expression,
    string: GraphQLString<'schema>,
}

enum PropertyLocation<'schema> {
    Source(String),
    Connect(ConnectHTTPCoordinate<'schema>),
}

#[allow(unused)]
pub(crate) struct UrlProperties<'schema> {
    location: PropertyLocation<'schema>,
    method: Option<Property<'schema>>,
    scheme: Option<Property<'schema>>,
    host: Option<Property<'schema>>,
    port: Option<Property<'schema>>,
    user: Option<Property<'schema>>,
    password: Option<Property<'schema>>,
    path: Option<Property<'schema>>,
    query: Option<Property<'schema>>,
}

impl<'schema> UrlProperties<'schema> {
    pub(in crate::sources::connect::validation) fn parse_for_connector(
        connector: ConnectHTTPCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Message> {
        Self::parse(PropertyLocation::Connect(connector), schema, http_arg)
    }

    pub(in crate::sources::connect::validation) fn parse_for_source(
        source_coordinate: String,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Message> {
        Self::parse(
            PropertyLocation::Source(source_coordinate),
            schema,
            http_arg,
        )
    }

    fn parse(
        location: PropertyLocation<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Message> {
        let mut method = None;
        let mut scheme = None;
        let mut host = None;
        let mut port = None;
        let mut user = None;
        let mut password = None;
        let mut path = None;
        let mut query = None;

        fn parse_http_arg<'schema>(
            node: &'schema Node<Value>,
            name: &'schema str,
            errors: &mut Vec<Message>,
            source_map: &'schema SourceMap,
        ) -> Option<Property<'schema>> {
            let Ok(string) = GraphQLString::new(node, source_map) else {
                errors.push(Message {
                    code: Code::InvalidUrlProperty,
                    message: format!("The `{name}` argument must be a string."),
                    locations: node.line_column_range(source_map).into_iter().collect(),
                });
                return None;
            };

            let selection = match JSONSelection::parse(string.as_str()) {
                Ok(selection) => selection,
                Err(e) => {
                    errors.push(Message {
                        code: Code::InvalidUrlProperty,
                        message: format!("The `{name}` argument is invalid: {e}"),
                        locations: node.line_column_range(source_map).into_iter().collect(),
                    });
                    return None;
                }
            };

            Some(Property {
                expression: Expression {
                    expression: selection,
                    location: 0..0,
                },
                string,
            })
        }

        let mut errors = Vec::new();
        let source_map = &schema.sources;

        for (name, value) in http_arg {
            match name.as_str() {
                "method" => method = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "scheme" => scheme = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "host" => host = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "port" => port = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "user" => user = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "password" => {
                    password = parse_http_arg(value, name.as_str(), &mut errors, source_map)
                }
                "path" => path = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "query" => query = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                _ => {}
            }
        }

        if !errors.is_empty() {
            let coordinate = match location {
                PropertyLocation::Source(source) => source,
                PropertyLocation::Connect(connector) => connector.to_string(),
            };

            // TODO return all errors individually
            return Err(Message {
                code: Code::InvalidUrlProperty,
                message: format!(
                    "{coordinate} has invalid syntax in URL properties.\n{}",
                    errors.iter().map(|e| e.message.clone()).join("\n")
                ),
                locations: errors.into_iter().flat_map(|err| err.locations).collect(),
            });
        }

        Ok(Self {
            location,
            method,
            scheme,
            host,
            port,
            user,
            password,
            path,
            query,
        })
    }

    pub(in crate::sources::connect::validation) fn type_check(
        &self,
        schema: &SchemaInfo<'_>,
    ) -> Vec<Message> {
        let mut messages = vec![];

        for (name, property, shape) in self {
            messages.extend(
                self.property_type_check(property, name, schema, shape)
                    .err(),
            );
        }

        messages
    }

    fn property_type_check(
        &self,
        property: Option<&Property>,
        name: &str,
        schema: &SchemaInfo<'_>,
        expected_shape: &Shape,
    ) -> Result<(), Message> {
        let Some(property) = property else {
            return Ok(());
        };

        let context = match &self.location {
            PropertyLocation::Source(_) => {
                expression::Context::for_source(schema, &property.string, Code::InvalidUrlProperty)
            }
            PropertyLocation::Connect(coord) => expression::Context::for_connect_request(
                schema,
                coord.connect_directive_coordinate,
                &property.string,
                Code::InvalidUrlProperty,
            ),
        };

        expression::validate(&property.expression, &context, expected_shape).map_err(|e| {
            let message = match &self.location {
                PropertyLocation::Source(source) => {
                    format!("In {source}, argument `{name}` is invalid: {}", e.message,)
                }
                PropertyLocation::Connect(coord) => {
                    format!("In {coord}, argument `{name}` is invalid: {}", e.message,)
                }
            };
            Message { message, ..e }
        })
    }
}

impl<'a, 's> IntoIterator for &'a UrlProperties<'s> {
    type Item = (
        &'static str,
        Option<&'a Property<'s>>,
        &'static LazyLock<Shape>,
    );
    type IntoIter = std::array::IntoIter<Self::Item, 8>;

    fn into_iter(self) -> Self::IntoIter {
        [
            ("method", self.method.as_ref(), &METHOD_SHAPE),
            ("scheme", self.scheme.as_ref(), &SCHEME_SHAPE),
            ("host", self.host.as_ref(), &HOST_SHAPE),
            ("port", self.port.as_ref(), &PORT_SHAPE),
            ("user", self.user.as_ref(), &USER_SHAPE),
            ("password", self.password.as_ref(), &PASSWORD_SHAPE),
            ("path", self.path.as_ref(), &PATH_SHAPE),
            ("query", self.query.as_ref(), &QUERY_SHAPE),
        ]
        .into_iter()
    }
}

static METHOD_SHAPE: LazyLock<Shape> = LazyLock::new(|| {
    Shape::one(
        [
            Shape::string_value("GET", None),
            Shape::string_value("POST", None),
            Shape::string_value("PUT", None),
            Shape::string_value("PATCH", None),
            Shape::string_value("DELETE", None),
        ],
        None,
    )
});

static SCHEME_SHAPE: LazyLock<Shape> = LazyLock::new(|| {
    Shape::one(
        [
            Shape::string_value("http", None),
            Shape::string_value("https", None),
        ],
        None,
    )
});

static HOST_SHAPE: LazyLock<Shape> = LazyLock::new(|| Shape::string([]));

static PORT_SHAPE: LazyLock<Shape> =
    LazyLock::new(|| Shape::one([Shape::int([]), Shape::null([])], []));

static USER_SHAPE: LazyLock<Shape> =
    LazyLock::new(|| Shape::one([Shape::string([]), Shape::null([])], []));

static PASSWORD_SHAPE: LazyLock<Shape> =
    LazyLock::new(|| Shape::one([Shape::string([]), Shape::null([])], []));

static PATH_SHAPE: LazyLock<Shape> = LazyLock::new(|| {
    Shape::list(
        Shape::one(
            [
                Shape::string([]),
                Shape::int([]),
                Shape::float([]),
                Shape::bool([]),
            ],
            [],
        ),
        [],
    )
});

static QUERY_SHAPE: LazyLock<Shape> = LazyLock::new(|| Shape::dict(Shape::unknown([]), []));
