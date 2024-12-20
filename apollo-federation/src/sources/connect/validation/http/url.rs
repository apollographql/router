use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::Node;
use shape::graphql;
use shape::Shape;
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
    let object_type = coordinate.connect.field_coordinate.object;
    let is_root_type = schema
        .schema_definition
        .query
        .as_ref()
        .is_some_and(|query| query.name == object_type.name)
        || schema
            .schema_definition
            .mutation
            .as_ref()
            .is_some_and(|mutation| mutation.name == object_type.name);
    let mut var_lookup: IndexMap<Namespace, Shape> = [
        (
            Namespace::Args,
            Shape::record(
                coordinate
                    .connect
                    .field_coordinate
                    .field
                    .arguments
                    .iter()
                    .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
                    .collect(),
            ),
        ),
        (Namespace::Config, Shape::none()),
        (Namespace::Context, Shape::none()),
    ]
    .into_iter()
    .collect();
    if !is_root_type {
        var_lookup.insert(Namespace::This, Shape::from(object_type.as_ref()));
    }

    for expression in template.expressions() {
        messages.extend(
            expression
                .validate(&shape_lookup, &var_lookup)
                .err()
                .into_iter()
                .flatten()
                .map(|err| Message {
                    code: Code::InvalidUrl,
                    message: format!("In {coordinate}: {}", err.message),
                    locations: str_value
                        .line_col_for_subslice(err.location, schema)
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
