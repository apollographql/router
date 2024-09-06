use std::str::FromStr;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Node;

use crate::sources::connect::validation::coordinates::UrlCoordinate;
use crate::sources::connect::validation::require_value_is_str;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::URLTemplate;

pub(crate) fn validate(
    value: &Node<Value>,
    coordinate: UrlCoordinate,
    sources: &SourceMap,
) -> Result<URLTemplate, Message> {
    let str_value = require_value_is_str(value, coordinate, sources)?;
    let template = URLTemplate::from_str(str_value).map_err(|err| Message {
        code: Code::InvalidUrl,
        message: format!("{coordinate} must be a valid URL. {err}",),
        locations: value.line_column_range(sources).into_iter().collect(),
    })?;

    todo!(Add a test for invalid templates);

    if let Some(base) = template.base.as_ref() {
        let scheme = base.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(Message {
                code: Code::InvalidUrlScheme,
                message: format!(
                    "The value {value} for {coordinate} must be http or https, got {scheme}.",
                ),
                locations: value.line_column_range(sources).into_iter().collect(),
            });
        }
    }

    Ok(template)
}
