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
) -> Result<URLTemplate, Message> {
    let str_value = require_value_is_str(value, coordinate, sources)?;
    let template =
        URLTemplate::from_str(str_value).map_err(|url_template::Error { message, location }| {
            Message {
                code: Code::InvalidUrl,
                message: format!("{coordinate} must be a valid URL template. {message}"),
                locations: location
                    .and_then(|location| select_substring_location(value, location, sources))
                    .or_else(|| value.line_column_range(sources))
                    .into_iter()
                    .collect(),
            }
        })?;

    if let Some(base) = template.base.as_ref() {
        validate_base_url(base, coordinate, value, sources)?;
    }

    Ok(template)
}

pub(crate) fn validate_base_url(
    url: &Url,
    coordinate: impl Display,
    value: &Node<Value>,
    sources: &SourceMap,
) -> Result<(), Message> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Message {
            code: Code::InvalidUrlScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: value.line_column_range(sources).into_iter().collect(),
        });
    }
    Ok(())
}

fn select_substring_location(
    value: &Node<Value>,
    substring_location: Range<usize>,
    sources: &SourceMap,
) -> Option<Range<LineColumn>> {
    let value_without_quotes = value.as_str()?;

    let source_span = value.location()?;
    let file = sources.get(&source_span.file_id())?;
    let source_text = file.source_text();
    let start_of_quotes = source_span.offset();
    let end_of_quotes = source_span.end_offset();
    let value_with_quotes = source_text.get(start_of_quotes..end_of_quotes)?;

    let len_of_starting_quotes = value_with_quotes.find(value_without_quotes)?;
    let len_of_ending_quotes =
        value_with_quotes.len() - value_without_quotes.len() - len_of_starting_quotes;

    let subslice_start_offset = start_of_quotes + len_of_starting_quotes + substring_location.start;
    let subslice_end_offset = end_of_quotes
        - len_of_ending_quotes
        - (value_without_quotes.len() - substring_location.end);

    let lookup = line_col::LineColLookup::new(source_text); // TODO: store and reuse
    let (line, column) = lookup.get(subslice_start_offset);
    let start = LineColumn { line, column };
    let (line, column) = lookup.get(subslice_end_offset);
    let end = LineColumn { line, column };

    Some(start..end)
}
