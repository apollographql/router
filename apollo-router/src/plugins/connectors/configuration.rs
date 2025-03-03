use std::collections::HashMap;
use std::sync::Arc;

use apollo_federation::sources::connect::CustomConfiguration;
use apollo_federation::sources::connect::expand::Connectors;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use super::incompatible::warn_incompatible_plugins;
use crate::Configuration;
use crate::plugins::connectors::plugin::PLUGIN_NAME;
use crate::services::connector_service::ConnectorSourceRef;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectorsConfig {
    /// A map of subgraph name to connectors config for that subgraph
    #[serde(default)]
    #[deprecated(note = "use `sources`")]
    pub(crate) subgraphs: HashMap<String, SubgraphConnectorConfiguration>,

    /// Map of subgraph_name.connector_source_name to source configuration
    #[serde(default)]
    pub(crate) sources: HashMap<String, SourceConfiguration>,

    /// Enables connector debugging information on response extensions if the feature is enabled
    #[serde(default)]
    pub(crate) debug_extensions: bool,

    /// The maximum number of requests for a connector source
    #[serde(default)]
    pub(crate) max_requests_per_operation_per_source: Option<usize>,

    /// When enabled, adds an entry to the context for use in coprocessors
    /// ```json
    /// {
    ///   "context": {
    ///     "entries": {
    ///       "apollo_connectors::sources_in_query_plan": [
    ///         { "subgraph_name": "subgraph", "source_name": "source" }
    ///       ]
    ///     }
    ///   }
    /// }
    /// ```
    #[serde(default)]
    pub(crate) expose_sources_in_context: bool,
}

// TODO: remove this after deprecation period
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

    /// The maximum number of requests for this source
    pub(crate) max_requests_per_operation: Option<usize>,

    /// Other values that can be used by connectors via `{$config.<key>}`
    #[serde(rename = "$config")]
    pub(crate) custom: CustomConfiguration,
}

/// Modifies connectors with values from the configuration
pub(crate) fn apply_config(
    router_config: &Configuration,
    mut connectors: Connectors,
) -> Connectors {
    // Enabling connectors might end up interfering with other router features, so we insert warnings
    // into the logs for any incompatibilities found.
    warn_incompatible_plugins(router_config, &connectors);

    let Some(config) = router_config.apollo_plugins.plugins.get(PLUGIN_NAME) else {
        return connectors;
    };
    let Ok(config) = serde_json::from_value::<ConnectorsConfig>(config.clone()) else {
        return connectors;
    };

    for connector in Arc::make_mut(&mut connectors.by_service_name).values_mut() {
        if let Ok(source_ref) = ConnectorSourceRef::try_from(&mut *connector) {
            if let Some(source_config) = config.sources.get(&source_ref.to_string()) {
                if let Some(url) = source_config.override_url.as_ref() {
                    connector.transport.source_url = Some(url.clone());
                }
                if let Some(max_requests) = source_config.max_requests_per_operation {
                    connector.max_requests = Some(max_requests);
                }
                connector.config = Some(source_config.custom.clone());
            }
        }

        // TODO: remove this after deprecation period
        #[allow(deprecated)]
        let Some(subgraph_config) = config.subgraphs.get(&connector.id.subgraph_name) else {
            continue;
        };
        if let Some(source_config) = connector
            .id
            .source_name
            .as_ref()
            .and_then(|source_name| subgraph_config.sources.get(source_name))
        {
            if let Some(url) = source_config.override_url.as_ref() {
                connector.transport.source_url = Some(url.clone());
            }
            if let Some(max_requests) = source_config.max_requests_per_operation {
                connector.max_requests = Some(max_requests);
            }
        }
        connector.config = Some(subgraph_config.custom.clone());
    }
    connectors
}
