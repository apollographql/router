use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Node;
use url::Url;

use crate::sources::connect::url_template;
use crate::sources::connect::url_template::VariableType;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::require_value_is_str;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::URLTemplate;
use crate::sources::connect::Variable;

pub(crate) fn validate_template(
    coordinate: HttpMethodCoordinate,
    sources: &SourceMap,
) -> Result<URLTemplate, Vec<Message>> {
    let (template, str_value) = match parse_template(coordinate, sources) {
        Ok(tuple) => tuple,
        Err(message) => return Err(vec![message]),
    };
    let mut messages = Vec::new();
    if let Some(base) = template.base.as_ref() {
        messages.extend(validate_base_url(
            base,
            coordinate,
            coordinate.node,
            str_value,
            sources,
        ));
    }

    for variable in template.path_variables() {
        if let Err(err) = validate_variable(variable, str_value, coordinate, sources) {
            messages.push(err);
        }
    }

    for variable in template.query_variables() {
        if let Err(err) = validate_variable(variable, str_value, coordinate, sources) {
            messages.push(err);
        }
    }

    // TODO: What happens with `?{$this.blah}`?
    // TODO: handle complex types of arguments/paths
    // TODO: hint at nullability requirements for path parameters

    if messages.is_empty() {
        Ok(template)
    } else {
        Err(messages)
    }
}

fn parse_template<'schema>(
    coordinate: HttpMethodCoordinate<'schema>,
    sources: &SourceMap,
) -> Result<(URLTemplate, &'schema str), Message> {
    let str_value = require_value_is_str(coordinate.node, coordinate, sources)?;
    let template =
        URLTemplate::from_str(str_value).map_err(|url_template::Error { message, location }| {
            Message {
                code: Code::InvalidUrl,
                message: format!("{coordinate} must be a valid URL template. {message}"),
                locations: select_substring_location(
                    coordinate.node.line_column_range(sources),
                    str_value,
                    location,
                ),
            }
        })?;
    Ok((template, str_value))
}

pub(crate) fn validate_base_url(
    url: &Url,
    coordinate: impl Display,
    value: &Node<Value>,
    str_value: &str,
    sources: &SourceMap,
) -> Option<Message> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        let scheme_location = Some(0..scheme.len());
        Some(Message {
            code: Code::InvalidUrlScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: select_substring_location(
                value.line_column_range(sources),
                str_value,
                scheme_location,
            ),
        })
    } else {
        None
    }
}

fn select_substring_location(
    line_column: Option<Range<LineColumn>>,
    full_url: &str,
    substring_location: Option<Range<usize>>,
) -> Vec<Range<LineColumn>> {
    line_column
        .map(|mut template_location| {
            // The default location includes the parameter name, we just want the value,
            // so we need to calculate that.
            template_location.end.column -= 1; // Get rid of the end quote
            template_location.start.column = template_location.end.column - full_url.len();

            if let Some(location) = substring_location {
                // We can point to a substring of the URL template! do it.
                template_location.start.column += location.start;
                template_location.end.column =
                    template_location.start.column + location.end - location.start;
            }
            template_location
        })
        .into_iter()
        .collect()
}

fn validate_variable(
    variable: &Variable,
    url_value: &str,
    coordinate: HttpMethodCoordinate,
    sources: &SourceMap,
) -> Result<(), Message> {
    let field_coordinate = coordinate.connect.field_coordinate;
    let field = field_coordinate.field;
    match variable.var_type {
        VariableType::Config => {} // We don't validate Router config yet
        VariableType::Args => {
            let arg_name = variable.path.split('.').next().unwrap_or(&variable.path);
            if !field.arguments.iter().any(|arg| arg.name == arg_name) {
                return Err(Message {
                    code: Code::UndefinedArgument,
                    message: format!(
                        "{coordinate} contains `{{{variable}}}`, but {field_coordinate} does not have an argument named `{arg_name}`.",
                    ),
                    locations: select_substring_location(
                        coordinate.node.line_column_range(sources),
                        url_value,
                        Some(variable.location.clone()),
                    )
                });
            }
        }
        VariableType::This => {
            let field_name = variable.path.split('.').next().unwrap_or(&variable.path);
            if !field_coordinate.object.fields.contains_key(field_name) {
                return Err(Message {
                    code: Code::UndefinedField,
                    message: format!(
                        "{coordinate} contains `{{{variable}}}`, but {object} does not have a field named `{field_name}`.",
                        object = field_coordinate.object.name,
                    ),
                    locations: select_substring_location(
                        coordinate.node.line_column_range(sources),
                        url_value,
                        Some(variable.location.clone()),
                    )
                });
            }
        }
    }
    Ok(())
}
