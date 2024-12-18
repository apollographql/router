use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::Node;
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

    let mut named_var_shapes = IndexMap::default();
    // TODO: don't set $this for root types
    named_var_shapes.insert(
        "$this",
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
    named_var_shapes.insert("$args", args);
    // TODO: should `any` be different than `none`?
    named_var_shapes.insert("$config", Shape::name("$config"));

    for expression in template.expressions() {
        // TODO: make sure this fails when missing vars and such
        let shape = expression
            .expression
            .compute_output_shape(Shape::none(), &named_var_shapes);
        messages.extend(
            validate_shape(&shape, &str_value, &expression, schema)
                .err()
                .into_iter(),
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
) -> Result<(), Message> {
    match shape.case() {
        ShapeCase::Array { .. } => Err(Message {
            code: Code::InvalidUrl,
            message: "URIs can't contain arrays".to_string(),
            locations: str_value
                .line_col_for_subslice(expression.location.clone(), schema)
                .into_iter()
                .collect(),
        }),
        ShapeCase::Object { .. } => Err(Message {
            code: Code::InvalidUrl,
            message: "URIs can't contain objects".to_string(),
            locations: str_value
                .line_col_for_subslice(expression.location.clone(), schema)
                .into_iter()
                .collect(),
        }),
        ShapeCase::One(shapes) | ShapeCase::All(shapes) => {
            for shape in shapes {
                validate_shape(shape, str_value, expression, schema)?;
            }
            Ok(())
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
