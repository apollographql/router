use std::collections::HashMap;
use std::sync::Arc;

use apollo_federation::sources::connect::expand::Connectors;
use apollo_federation::sources::connect::CustomConfiguration;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::plugins::connectors::plugin::PLUGIN_NAME;
use crate::Configuration;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectorsConfig {
    /// A map of subgraph name to connectors config for that subgraph
    #[serde(default)]
    pub(crate) subgraphs: HashMap<String, SubgraphConnectorConfiguration>,

    /// Enables connector debugging information on response extensions if the feature is enabled
    #[serde(default)]
    pub(crate) debug_extensions: bool,

    /// Set an upper bound on the number of requests genereated by a single operation
    /// to a specific source (`@source(name:)`) to avoid overloading an upstream service
    #[serde(default = "default_max_requests_per_source_and_operation")]
    pub(crate) default_max_requests_per_source_and_operation: u32,
}

/// Configuration for a connector subgraph
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphConnectorConfiguration {
    /// A map of `@source(name:)` to configuration for that source
    pub(crate) sources: HashMap<String, SourceConfiguration>,

    /// Other values that can be used by connectors via `{$config.<key>}`
    #[serde(rename = "$config")]
    pub(crate) custom: CustomConfiguration,
}

/// Configuration for a `@source` directive
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SourceConfiguration {
    /// Override the `@source(http: {baseURL:})`
    pub(crate) override_url: Option<Url>,

    /// Set an upper bound on the number of requests genereated by a single operation
    /// for this source to avoid overloading the upstream resources
    #[serde(default)]
    pub(crate) max_requests_per_operation: u32,
}

/// Modifies connectors with values from the configuration
pub(crate) fn apply_config(config: &Configuration, mut connectors: Connectors) -> Connectors {
    let Some(config) = config.apollo_plugins.plugins.get(PLUGIN_NAME) else {
        return connectors;
    };
    let Ok(config) = serde_json::from_value::<ConnectorsConfig>(config.clone()) else {
        return connectors;
    };

    for connector in Arc::make_mut(&mut connectors.by_service_name).values_mut() {
        let Some(subgraph_config) = config.subgraphs.get(&connector.id.subgraph_name) else {
            continue;
        };
        let source_config = connector
            .id
            .source_name
            .as_ref()
            .and_then(|source_name| subgraph_config.sources.get(source_name));
        if let Some(url) =
            source_config.and_then(|source_config| source_config.override_url.as_ref())
        {
            connector.transport.source_url = Some(url.clone());
        }

        connector.config = Some(subgraph_config.custom.clone());

        connector.max_requests_per_operation = source_config
            .map(|source_config| source_config.max_requests_per_operation)
            .unwrap_or(config.default_max_requests_per_source_and_operation);
    }
    connectors
}

fn default_max_requests_per_source_and_operation() -> u32 {
    100
}
