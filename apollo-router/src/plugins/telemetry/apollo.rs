//! Configuration for apollo telemetry.
// This entire file is license key functionality
use http::header::HeaderName;
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

use crate::plugin::serde::deserialize_header_name;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// The Apollo Studio endpoint for exporting traces and metrics.
    #[schemars(with = "Option<String>")]
    pub(crate) endpoint: Option<Url>,

    /// The Apollo Studio API key.
    #[schemars(skip)]
    #[serde(skip, default = "apollo_key")]
    pub(crate) apollo_key: Option<String>,

    /// The Apollo Studio graph reference.
    #[schemars(skip)]
    #[serde(skip, default = "apollo_graph_reference")]
    pub(crate) apollo_graph_ref: Option<String>,

    /// The name of the header to extract from requests when populating 'client nane' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_name_header_default_str")]
    #[serde(
        deserialize_with = "deserialize_header_name",
        default = "client_name_header_default"
    )]
    pub(crate) client_name_header: HeaderName,

    /// The name of the header to extract from requests when populating 'client version' for traces and metrics in Apollo Studio.
    #[schemars(with = "Option<String>", default = "client_version_header_default_str")]
    #[serde(
        deserialize_with = "deserialize_header_name",
        default = "client_version_header_default"
    )]
    pub(crate) client_version_header: HeaderName,

    /// The buffer size for sending traces to Apollo. Increase this if you are experiencing lost traces.
    #[serde(default = "default_buffer_size")]
    pub(crate) buffer_size: usize,

    /// Disable ftv1 tracing. ftv1 tracing can cause performance issues.
    /// This means that traces will not appear in Apollo Studio.
    #[serde(default)]
    pub(crate) disable_ftv1: bool,

    // This'll get overridden if a user tries to set it.
    // The purpose is to allow is to pass this in to the plugin.
    #[schemars(skip)]
    pub(crate) schema_id: String,
}

fn apollo_key() -> Option<String> {
    std::env::var("APOLLO_KEY").ok()
}

fn apollo_graph_reference() -> Option<String> {
    std::env::var("APOLLO_GRAPH_REF").ok()
}

fn client_name_header_default_str() -> &'static str {
    "apollographql-client-name"
}

fn client_name_header_default() -> HeaderName {
    HeaderName::from_static(client_name_header_default_str())
}

fn client_version_header_default_str() -> &'static str {
    "apollographql-client-version"
}

fn client_version_header_default() -> HeaderName {
    HeaderName::from_static(client_version_header_default_str())
}

fn default_buffer_size() -> usize {
    10000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: None,
            apollo_key: None,
            apollo_graph_ref: None,
            client_name_header: client_name_header_default(),
            client_version_header: client_version_header_default(),
            schema_id: "<no_schema_id>".to_string(),
            buffer_size: 10000,
            disable_ftv1: false,
        }
    }
}
