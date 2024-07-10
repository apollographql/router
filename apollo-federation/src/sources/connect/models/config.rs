use std::collections::HashMap;

#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// Configuration for a connector subgraph
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct SubgraphConnectorConfiguration {
    /// A map of `@source(name:)` to configuration for that source
    pub sources: HashMap<String, SourceConfiguration>,

    /// Other values that can be used by connectors via `{$config.<key>}`
    pub(crate) custom: CustomConfiguration,
}

pub type CustomConfiguration = HashMap<String, Value>;

/// Configuration for a `@source` directive
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct SourceConfiguration {
    /// Override the `@source(http: {baseURL:})`
    pub override_url: Option<Url>,
}
