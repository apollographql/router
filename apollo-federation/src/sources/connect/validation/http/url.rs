use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::Node;
use shape::graphql;
use shape::Shape;
use shape::ShapeCase;
use url::Url;

use crate::sources::connect::string_template;
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
    let shape_lookup = graphql::shapes_for_schema(schema);

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
            validate_shape(&shape, &shape_lookup, coordinate)
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
    shape_lookup: &IndexMap<&str, Shape>,
    coordinate: HttpMethodCoordinate,
) -> Result<(), String> {
    // TODO: don't include $this for root types
    let namespaces = [
        Namespace::Args,
        Namespace::Config,
        Namespace::Context,
        Namespace::This,
    ];
    match shape.case() {
        ShapeCase::Array { .. } => Err("URIs can't contain arrays".to_string()),
        ShapeCase::Object { .. } => Err("URIs can't contain objects".to_string()),
        ShapeCase::One(shapes) | ShapeCase::All(shapes) => {
            for shape in shapes {
                validate_shape(shape, shape_lookup, coordinate)?;
            }
            Ok(())
        }
        ShapeCase::Name(name, key) => {
            let mut shape = if name.starts_with('$') {
                let namespace = Namespace::from_str(name).map_err(|_| {
                    format!(
                        "unknown variable `{name}`, must be one of {namespaces}",
                        namespaces = namespaces.map(|ns| ns.as_str()).join(", ")
                    )
                })?;
                match namespace {
                    Namespace::Args => Shape::record(
                        coordinate
                            .connect
                            .field_coordinate
                            .field
                            .arguments
                            .iter()
                            .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
                            .collect(),
                    ),
                    Namespace::Context | Namespace::Config => Shape::null(), // Can't use none because that messes up the child check later
                    Namespace::This => {
                        Shape::from(coordinate.connect.field_coordinate.object.as_ref())
                    } // TODO: not for root types
                    Namespace::Status => {
                        return Err(format!(
                            "{namespace} is not allowed here, must be one of {namespaces}",
                            namespaces = namespaces.map(|ns| ns.as_str()).join(", "),
                        ))
                    }
                }
            } else {
                shape_lookup
                    .get(name.as_str())
                    .cloned()
                    .ok_or_else(|| format!("unknown type `{name}`"))?
            };
            let mut path = name.clone();
            for key in key {
                let child = shape.child(key);
                if child.is_none() {
                    return Err(format!("`{path}` doesn't have a field named `{key}`"));
                }
                shape = child;
                path = format!("{path}.{key}");
            }
            validate_shape(&shape, shape_lookup, coordinate)
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
