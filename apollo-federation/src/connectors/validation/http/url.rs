use std::fmt::Display;

use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use http::uri::Scheme;

use crate::connectors::StringTemplate;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::graphql::subslice_location;

pub(crate) fn validate_url_scheme(
    template: &StringTemplate,
    coordinate: impl Display,
    value: &Node<Value>,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    // Evaluate the template, replacing all dynamic expressions with empty strings. This should result in a valid
    // URL because of the URL building logic in `interpolate_uri`, even if the result is illogical with missing values.
    let (url, _) = template
        .interpolate_uri(&Default::default())
        .map_err(|err| Message {
            message: format!("In {coordinate}: {err}"),
            code: Code::InvalidUrl,
            locations: value
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })?;
    let Some(scheme) = url.scheme() else {
        return Err(Message {
            code: Code::InvalidUrlScheme,
            message: format!("Base URL for {coordinate} did not start with http:// or https://.",),
            locations: value
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    };
    if *scheme == Scheme::HTTP || *scheme == Scheme::HTTPS {
        Ok(())
    } else {
        let scheme_location = 0..scheme.as_str().len();
        Err(Message {
            code: Code::InvalidUrlScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: subslice_location(value, scheme_location, schema)
                .into_iter()
                .collect(),
        })
    }
}
