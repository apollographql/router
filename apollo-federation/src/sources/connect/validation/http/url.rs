use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Node;
use url::Url;

use crate::sources::connect::url_template;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::require_value_is_str;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::URLTemplate;

pub(crate) fn validate_template(
    value: &Node<Value>,
    coordinate: HttpMethodCoordinate,
    sources: &SourceMap,
) -> Result<URLTemplate, Vec<Message>> {
    let (template, str_value) = match parse_template(value, coordinate, sources) {
        Ok(tuple) => tuple,
        Err(message) => return Err(vec![message]),
    };
    let mut messages = Vec::new();
    if let Some(base) = template.base.as_ref() {
        messages.extend(validate_base_url(
            base, coordinate, value, str_value, sources,
        ));
    }

    Ok(template)
}

pub(crate) fn parse_template<'schema>(
    value: &'schema Node<Value>,
    coordinate: HttpMethodCoordinate,
    sources: &SourceMap,
) -> Result<(URLTemplate, &'schema str), Message> {
    let str_value = require_value_is_str(value, coordinate, sources)?;
    let template =
        URLTemplate::from_str(str_value).map_err(|url_template::Error { message, location }| {
            Message {
                code: Code::InvalidUrl,
                message: format!("{coordinate} must be a valid URL template. {message}"),
                locations: select_substring_location(
                    value.line_column_range(sources),
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
