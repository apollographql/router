use std::fmt::Display;

use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use url::Url;

use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;

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
