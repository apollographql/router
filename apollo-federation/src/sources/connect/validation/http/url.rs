use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::Node;
use url::Url;

use crate::sources::connect::string_template;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::selection::validate_selection_variables;
use crate::sources::connect::validation::variable::VariableResolver;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::variable::ConnectorsContext;
use crate::sources::connect::variable::Phase;
use crate::sources::connect::variable::Target;
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

    let expression_context =
        ConnectorsContext::new(coordinate.connect.into(), Phase::Request, Target::Url);
    let variable_resolver = VariableResolver::new(expression_context, schema);
    for expression in template.expressions() {
        messages.extend(
            validate_selection_variables(
                &variable_resolver,
                coordinate,
                &expression.expression,
                str_value.slice(expression.location.clone()),
            )
            .err(),
        );
        // TODO: type check the expression. It can't resolve to a list or object.
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
