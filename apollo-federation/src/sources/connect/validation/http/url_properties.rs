use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use shape::Shape;

use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::SchemaInfo;
use crate::sources::connect::validation::coordinates::ConnectHTTPCoordinate;
use crate::sources::connect::validation::coordinates::SourceDirectiveCoordinate;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::MappingArgument;
use crate::sources::connect::validation::expression::parse_mapping_argument;

enum PropertyLocation<'schema> {
    Source(SourceDirectiveCoordinate<'schema>),
    Connect(ConnectHTTPCoordinate<'schema>),
}

#[allow(unused)]
pub(in crate::sources::connect::validation) struct UrlProperties<'schema> {
    location: PropertyLocation<'schema>,
    path: Option<MappingArgument<'schema>>,
    query_params: Option<MappingArgument<'schema>>,
}

impl<'schema> UrlProperties<'schema> {
    pub(in crate::sources::connect::validation) fn parse_for_connector(
        connector: ConnectHTTPCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Vec<Message>> {
        Self::parse(PropertyLocation::Connect(connector), schema, http_arg)
    }

    pub(in crate::sources::connect::validation) fn parse_for_source(
        source_coordinate: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Vec<Message>> {
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
    ) -> Result<Self, Vec<Message>> {
        let mut path = None;
        let mut query_params = None;

        let mut errors = Vec::new();
        let source_map = &schema.sources;

        let coordinate = match &location {
            PropertyLocation::Source(source) => source.to_string(),
            PropertyLocation::Connect(connector) => connector.to_string(),
        };
        let prefix = format!("In {coordinate}");

        for (name, value) in http_arg {
            match name.as_str() {
                "path" => match parse_mapping_argument(
                    value,
                    &prefix,
                    Some(name.as_str()),
                    Code::InvalidUrlProperty,
                    source_map,
                ) {
                    Ok(p) => path = Some(p),
                    Err(e) => errors.push(e),
                },
                "queryParams" => {
                    match parse_mapping_argument(
                        value,
                        &prefix,
                        Some(name.as_str()),
                        Code::InvalidUrlProperty,
                        source_map,
                    ) {
                        Ok(qp) => query_params = Some(qp),
                        Err(e) => errors.push(e),
                    }
                }
                _ => {}
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self {
            location,
            path,
            query_params,
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
        property: Option<&MappingArgument>,
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
        Option<&'a MappingArgument<'s>>,
        &'static LazyLock<Shape>,
    );
    type IntoIter = std::array::IntoIter<Self::Item, 2>;

    fn into_iter(self) -> Self::IntoIter {
        [
            ("path", self.path.as_ref(), &PATH_SHAPE),
            ("queryParams", self.query_params.as_ref(), &QUERY_SHAPE),
        ]
        .into_iter()
    }
}

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
