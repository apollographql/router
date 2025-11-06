use std::fmt;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use itertools::Itertools;
use shape::Shape;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::SchemaInfo;
use crate::connectors::validation::coordinates::ConnectDirectiveCoordinate;
use crate::connectors::validation::coordinates::SourceDirectiveCoordinate;
use crate::connectors::validation::expression;
use crate::connectors::validation::expression::MappingArgument;
use crate::connectors::validation::expression::parse_mapping_argument;

pub(in crate::connectors::validation) struct UrlProperties<'schema> {
    properties: Vec<Property<'schema>>,
}

impl<'schema> UrlProperties<'schema> {
    pub(in crate::connectors::validation) fn parse_for_connector(
        connector: ConnectDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Vec<Message>> {
        Self::parse(&ConnectOrSource::Connect(connector), schema, http_arg)
    }

    pub(in crate::connectors::validation) fn parse_for_source(
        source_coordinate: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Vec<Message>> {
        Self::parse(
            &ConnectOrSource::Source(source_coordinate),
            schema,
            http_arg,
        )
    }

    fn parse(
        directive: &ConnectOrSource<'schema>,
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Vec<Message>> {
        let (properties, errors): (Vec<Property>, Vec<Message>) = http_arg
            .iter()
            .filter_map(|(name, value)| {
                PropertyName::iter()
                    .find(|prop_name| prop_name.as_str() == name.as_str())
                    .map(|name| (name, value))
            })
            .map(|(property, value)| {
                let coordinate = Coordinate {
                    directive: directive.clone(),
                    property,
                };
                let mapping =
                    parse_mapping_argument(value, &coordinate, Code::InvalidUrlProperty, schema)?;
                Ok(Property {
                    coordinate,
                    mapping,
                })
            })
            .partition_result();

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self { properties })
    }

    pub(in crate::connectors::validation) fn type_check(
        &self,
        schema: &SchemaInfo<'_>,
    ) -> Vec<Message> {
        let mut messages = vec![];

        for property in &self.properties {
            messages.extend(self.property_type_check(property, schema).err());
        }

        messages
    }

    fn property_type_check(
        &self,
        property: &Property<'_>,
        schema: &SchemaInfo<'_>,
    ) -> Result<(), Message> {
        let context = match property.coordinate.directive {
            ConnectOrSource::Source(_) => expression::Context::for_source(
                schema,
                &property.mapping.node,
                Code::InvalidUrlProperty,
            ),
            ConnectOrSource::Connect(coord) => expression::Context::for_connect_request(
                schema,
                coord,
                &property.mapping.node,
                Code::InvalidUrlProperty,
            ),
        };

        expression::validate(
            &property.mapping.expression,
            &context,
            property.expected_shape(),
        )
        .map_err(|e| {
            let message = format!("{} is invalid: {}", property.coordinate, e.message);
            Message { message, ..e }
        })
    }
}

struct Property<'schema> {
    coordinate: Coordinate<'schema>,
    mapping: MappingArgument,
}

impl Property<'_> {
    fn expected_shape(&self) -> &Shape {
        match self.coordinate.property {
            PropertyName::Path => &PATH_SHAPE,
            PropertyName::QueryParams => &QUERY_SHAPE,
        }
    }
}

#[derive(Clone)]
struct Coordinate<'schema> {
    directive: ConnectOrSource<'schema>,
    property: PropertyName,
}

impl fmt::Display for Coordinate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.directive {
            ConnectOrSource::Source(source) => {
                write!(f, "In {source}, the `{}` argument", self.property)
            }
            ConnectOrSource::Connect(connect) => {
                write!(f, "In {connect}, the `{}` argument", self.property)
            }
        }
    }
}

#[derive(Clone)]
enum ConnectOrSource<'schema> {
    Source(SourceDirectiveCoordinate<'schema>),
    Connect(ConnectDirectiveCoordinate<'schema>),
}

impl fmt::Display for ConnectOrSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConnectOrSource::Source(source) => write!(f, "{source}"),
            ConnectOrSource::Connect(connect) => write!(f, "{connect}"),
        }
    }
}

#[derive(Clone, Copy, EnumIter)]
enum PropertyName {
    Path,
    QueryParams,
}

impl PropertyName {
    const fn as_str(&self) -> &'static str {
        match self {
            PropertyName::Path => "path",
            PropertyName::QueryParams => "queryParams",
        }
    }
}

impl fmt::Display for PropertyName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.as_str())
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
