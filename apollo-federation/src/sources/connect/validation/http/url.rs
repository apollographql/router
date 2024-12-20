use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::Node;
use itertools::Itertools;
use shape::graphql;
use shape::Shape;
use shape::ShapeCase;
use url::Url;

use crate::sources::connect::string_template;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::Namespace;
use crate::sources::connect::URLTemplate;

pub(crate) fn validate_template(
    coordinate: HttpMethodCoordinate,
    schema: &SchemaInfo,
) -> Result<URLTemplate, Vec<Message>> {
    let (template, str_value) = match parse_template(coordinate, schema) {
        Ok(tuple) => tuple,
        Err(message) => return Err(vec![message]),
    };
    let mut messages = Vec::new();
    if let Some(base) = template.base.as_ref() {
        messages
            .extend(validate_base_url(base, coordinate, coordinate.node, str_value, schema).err());
    }

    // TODO: Compute this once for the whole subgraph
    let mut shape_lookup = graphql::shapes_for_schema(schema);
    // TODO: don't set $this for root types
    shape_lookup.insert(
        Namespace::This.as_str(),
        Shape::from(coordinate.connect.field_coordinate.object.as_ref()),
    );
    let args = Shape::record(
        coordinate
            .connect
            .field_coordinate
            .field
            .arguments
            .iter()
            .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
            .collect(),
    );
    shape_lookup.insert(Namespace::Args.as_str(), args);
    shape_lookup.insert(
        Namespace::Config.as_str(),
        Shape::object(Default::default(), Shape::any()),
    );

    for expression in template.expressions() {
        let shape = expression
            .expression
            .compute_output_shape(Shape::none(), &Default::default());
        messages.extend(shape.errors().map(|error| {
            Message {
                code: Code::InvalidUrl,
                message: error.message.clone(),
                locations: str_value
                    .line_col_for_subslice(
                        error.range.as_ref().unwrap_or(&expression.location).clone(),
                        schema,
                    )
                    .into_iter()
                    .collect(),
            }
        }));
        messages.extend(
            validate_shape(&shape, &str_value, expression, schema, &shape_lookup)
                .err()
                .map(|message| Message {
                    code: Code::InvalidUrl,
                    message: format!("In {coordinate}: {message}"),
                    locations: str_value
                        .line_col_for_subslice(expression.location.clone(), schema)
                        .into_iter()
                        .collect(),
                }),
        );
    }

    if messages.is_empty() {
        Ok(template)
    } else {
        Err(messages)
    }
}

fn validate_shape(
    shape: &Shape,
    str_value: &GraphQLString,
    expression: &Expression,
    schema: &SchemaInfo,
    shape_lookup: &IndexMap<&str, Shape>,
) -> Result<(), String> {
    match shape.case() {
        ShapeCase::Array { .. } => Err("URIs can't contain arrays".to_string()),
        ShapeCase::Object { .. } => Err("URIs can't contain objects".to_string()),
        ShapeCase::One(shapes) | ShapeCase::All(shapes) => {
            for shape in shapes {
                validate_shape(shape, str_value, expression, schema, shape_lookup)?;
            }
            Ok(())
        }
        ShapeCase::Name(name, key) => {
            let Some(mut shape) = shape_lookup.get(name.as_str()).cloned() else {
                return Err(if name.starts_with('$') {
                    format!(
                        "unknown variable `{name}`, must be one of {namespaces}",
                        namespaces = shape_lookup
                            .keys()
                            .filter(|key| key.starts_with('$'))
                            .join(", ")
                    )
                } else {
                    format!("unknown type `{name}`")
                });
            };
            for key in key {
                let child = shape.child(key);
                if child.is_none() {
                    return Err(format!("`{name}` doesn't have a field named `{key}`"));
                }
                shape = child;
            }
            validate_shape(&shape, str_value, expression, schema, shape_lookup)
        }
        _ => Ok(()),
    }
}

fn parse_template<'schema>(
    coordinate: HttpMethodCoordinate<'schema>,
    schema: &'schema SchemaInfo,
) -> Result<(URLTemplate, GraphQLString<'schema>), Message> {
    let str_value = GraphQLString::new(coordinate.node, &schema.sources).map_err(|_| Message {
        code: Code::GraphQLError,
        message: format!("The value for {coordinate} must be a string."),
        locations: coordinate
            .node
            .line_column_range(&schema.sources)
            .into_iter()
            .collect(),
    })?;
    let template = URLTemplate::from_str(str_value.as_str()).map_err(
        |string_template::Error { message, location }| Message {
            code: Code::InvalidUrl,
            message: format!("In {coordinate}: {message}"),
            locations: str_value
                .line_col_for_subslice(location, schema)
                .into_iter()
                .collect(),
        },
    )?;
    Ok((template, str_value))
}

pub(crate) fn validate_base_url(
    url: &Url,
    coordinate: impl Display,
    value: &Node<Value>,
    str_value: GraphQLString,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        let scheme_location = 0..scheme.len();
        Err(Message {
            code: Code::InvalidUrlScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: str_value
                .line_col_for_subslice(scheme_location, schema)
                .into_iter()
                .collect(),
        })
    } else {
        Ok(())
    }
}
