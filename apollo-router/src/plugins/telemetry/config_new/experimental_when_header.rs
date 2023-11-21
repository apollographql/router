// Note that this configuration will be removed when events are implemented.

use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::plugin::serde::deserialize_regex;
use crate::services::SupergraphRequest;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum HeaderLoggingCondition {
    /// Match header value given a regex to display logs
    Matching {
        /// Header name
        name: String,
        /// Regex to match the header value
        #[schemars(with = "String", rename = "match")]
        #[serde(deserialize_with = "deserialize_regex", rename = "match")]
        matching: Regex,
        /// Display request/response headers (default: false)
        #[serde(default)]
        headers: bool,
        /// Display request/response body (default: false)
        #[serde(default)]
        body: bool,
    },
    /// Match header value given a value to display logs
    Value {
        /// Header name
        name: String,
        /// Header value
        value: String,
        /// Display request/response headers (default: false)
        #[serde(default)]
        headers: bool,
        /// Display request/response body (default: false)
        #[serde(default)]
        body: bool,
    },
}

impl HeaderLoggingCondition {
    /// Returns if we should display the request/response headers and body given the `SupergraphRequest`
    pub(crate) fn should_log(&self, req: &SupergraphRequest) -> (bool, bool) {
        match self {
            HeaderLoggingCondition::Matching {
                name,
                matching: matched,
                headers,
                body,
            } => {
                let header_match = req
                    .supergraph_request
                    .headers()
                    .get(name)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| matched.is_match(h))
                    .unwrap_or_default();

                if header_match {
                    (*headers, *body)
                } else {
                    (false, false)
                }
            }
            HeaderLoggingCondition::Value {
                name,
                value,
                headers,
                body,
            } => {
                let header_match = req
                    .supergraph_request
                    .headers()
                    .get(name)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| value.as_str() == h)
                    .unwrap_or_default();

                if header_match {
                    (*headers, *body)
                } else {
                    (false, false)
                }
            }
        }
    }
}
