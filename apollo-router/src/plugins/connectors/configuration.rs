use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_federation::sources::connect::expand::Connectors;
use apollo_federation::sources::connect::CustomConfiguration;
use itertools::Itertools as _;
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
}

/// Modifies connectors with values from the configuration
pub(crate) fn apply_config(
    router_config: &Configuration,
    mut connectors: Connectors,
) -> Connectors {
    // Enabling connectors might end up interfering with other router features, so we insert warnings
    // into the logs for any incompatibilites found.
    warn_incompatible_plugins(router_config, &connectors);

    let Some(config) = router_config.apollo_plugins.plugins.get(PLUGIN_NAME) else {
        return connectors;
    };
    let Ok(config) = serde_json::from_value::<ConnectorsConfig>(config.clone()) else {
        return connectors;
    };

    for connector in Arc::make_mut(&mut connectors.by_service_name).values_mut() {
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

/// Warn about possible incompatibilities with other router features / plugins.
///
/// Connectors do not currently work with some of the existing router
/// features, so we need to inform the user when those features are
/// detected as being enabled.
fn warn_incompatible_plugins(config: &Configuration, connectors: &Connectors) {
    /// Generate a consistent warning message for a specified plugin
    fn msg(plugin_name: &str) -> String {
        format!("plugin `{}` is enabled for connector-enabled subgraphs, which is not yet supported. See https://go.apollo.dev/INSERT_DOCS_LINK for more info", plugin_name)
    }

    let connector_enabled_subgraphs: HashSet<&String> = connectors
        .by_service_name
        .values()
        .map(|v| &v.id.subgraph_name)
        .collect();

    // If we don't have any connector-enabled subgraphs, then no need to warn
    if connector_enabled_subgraphs.is_empty() {
        return;
    }

    // Plugins tend to have a few forms of specifying which subgraph to target:
    // - A catch-all `all`
    // - By the subgraph's actual name under a `subgraphs` key
    // In either case, the configuration will have a prefix which then ends in the
    // target subgraph, so we keep track of all of those prefixes which aren't
    // supported by connector-enabled subgraphs.
    //
    // TODO: Each of these config options come from a corresponding plugin which
    // is identified by its name. These are currently hardcoded here, so it'd be
    // nice to extract them from the plugins themselves...
    let incompatible_prefixes = [
        ("authentication", ["subgraph"].as_slice()),
        ("batching", &["subgraph"]),
        ("coprocessor", &["subgraph"]),
        ("headers", &[]),
        (
            "telemetry",
            &["exporters", "metrics", "common", "attributes", "subgraph"],
        ),
        ("preview_entity_cache", &["subgraph"]),
        ("telemetry", &["apollo", "errors", "subgraph"]),
        ("traffic_shaping", &[]),
    ];

    for (plugin_name, prefix) in incompatible_prefixes {
        // Plugin configuration is only populated if the user has specified it,
        // so we can skip any that are missing.
        let Some(plugin_config) = config.apollo_plugins.plugins.get(plugin_name) else {
            continue;
        };

        // Drill into the prefix
        let Some(prefixed_config) = prefix
            .iter()
            .try_fold(plugin_config, |acc, next| acc.get(next))
        else {
            continue;
        };

        // Check if any of the connector enabled subgraphs are targeted
        let incompatible_subgraphs = if prefixed_config.get("all").is_some() {
            // If all is configured, then all connector-enabled subgraphs are affected.
            &connector_enabled_subgraphs
        } else if let Some(subgraphs) = prefixed_config.get("subgraphs") {
            // Otherwise, we'll need to do a set intersection between the list of connector-enabled
            // subgraphs and configured subgraphs to see which, if any, are affected.
            let configured = subgraphs
                .as_object()
                .map(|o| o.keys().collect())
                .unwrap_or(HashSet::new());

            &configured
                .intersection(&connector_enabled_subgraphs)
                .copied()
                .collect()
        } else {
            &HashSet::new()
        };

        if !incompatible_subgraphs.is_empty() {
            tracing::warn!(
                subgraphs = incompatible_subgraphs.iter().join(","),
                message = msg(plugin_name)
            );
        }
    }

    // Lastly, there are a few plugins which influence all subgraphs, regardless
    // of configuration, so we warn about these statically here if we have
    // any connector-enabled subgraphs.
    let incompatible_plugins = ["demand_control", "rhai"];
    for plugin_name in incompatible_plugins {
        if config.apollo_plugins.plugins.get(plugin_name).is_some() {
            tracing::warn!(
                subgraphs = connector_enabled_subgraphs.iter().join(","),
                message = msg(plugin_name)
            );
        }
    }
}
