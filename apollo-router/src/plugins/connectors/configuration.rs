use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

/// Connectors configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Connectors {
    /// Per subgraph configuration
    pub(crate) subgraphs: HashMap<String, HashMap<String, SourceApiConfiguration>>,
}

/// Configuration for a connector subgraph
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphConnectorConfiguration {
    /// Per API configuration
    pub(crate) apis: HashMap<String, SourceApiConfiguration>,
}

/// Configuration for a source API
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SourceApiConfiguration {
    /// Override the base URL for the API
    pub(crate) override_url: Option<Url>,
}
