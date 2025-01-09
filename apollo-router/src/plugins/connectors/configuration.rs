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
        format!("plugin `{}` is enabled for connector-enabled subgraphs, which is currently unsupported. See https://go.apollo.dev/connectors/incompat for more info", plugin_name)
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
    //
    // Note: Some of these also allow for enabling / disabling (and overriding
    // global options), so we need to know if that is the case when collecting
    // which subgraphs might trigger incompatibilities.
    struct IncompatiblePlugin {
        enabled: bool,
        /// If the configuration allows for overriding on a subgraph-level whether
        /// to enable or disable a feature.
        subgraph_can_override: bool,
        /// The name of the plugin
        plugin: &'static str,
        /// The set of keys needed to drill through to reach the subgraph
        /// configuration.
        drill_prefix: &'static [&'static str],
    }
    let incompatible_prefixes = [
        IncompatiblePlugin {
            enabled: config.apq.enabled,
            subgraph_can_override: true,
            plugin: "apq",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "authentication",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: config.batching.enabled,
            subgraph_can_override: true,
            plugin: "batching",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "coprocessor",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "headers",
            drill_prefix: &[],
        },
        IncompatiblePlugin {
            enabled: config
                .apollo_plugins
                .plugins
                .get("preview_entity_cache")
                .and_then(|p| p.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_default(),
            subgraph_can_override: true,
            plugin: "preview_entity_cache",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "telemetry",
            drill_prefix: &["apollo", "errors", "subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "telemetry",
            drill_prefix: &["exporters", "metrics", "common", "attributes", "subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "tls",
            drill_prefix: &["subgraph"],
        },
        IncompatiblePlugin {
            enabled: true,
            subgraph_can_override: false,
            plugin: "traffic_shaping",
            drill_prefix: &[],
        },
    ];

    // Note: Some of the incopmpatible plugins are hoisted up into individual
    // properties on the config object, so we operate on the actual yaml to
    // consolidate how we handle core features vs arbitrary plugins.
    //
    // Note: Execution of this entire chain of validation methods won't happen
    // if the configuration is invalid, but we add a check just in case for
    // debug builds.
    let Some(raw_config) = config
        .validated_yaml
        .as_ref()
        .and_then(serde_json::Value::as_object)
    else {
        debug_assert!(
            false,
            "configuration was invalid, which should not have happened"
        );

        return;
    };

    for IncompatiblePlugin {
        enabled,
        subgraph_can_override,
        plugin,
        drill_prefix,
    } in incompatible_prefixes
    {
        // If the plugin is not enabled, no need to process it
        if !enabled {
            continue;
        }

        // Plugin configuration is only populated if the user has specified it,
        // so we can skip any that are missing.
        let Some(plugin_config) = raw_config.get(plugin) else {
            continue;
        };

        // Drill into the prefix
        let Some(prefixed_config) = drill_prefix
            .iter()
            .try_fold(plugin_config, |acc, next| acc.get(next))
        else {
            continue;
        };

        // Grab all of the configured subgraphs for this plugin
        // Note: If the plugin supports overriding on a per-subgraph level, then
        // we'll need to partition based on an enabled flag per subgraph.
        let empty = serde_json::Map::new();
        let configured = prefixed_config
            .get("subgraphs")
            .and_then(serde_json::Value::as_object)
            .unwrap_or(&empty);

        let (enabled, disabled): (HashSet<&String>, HashSet<&String>) =
            configured.iter().partition_map(|(name, subgraph_conf)| {
                if subgraph_can_override {
                    let enabled = subgraph_conf
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or_default();
                    match enabled {
                        true => itertools::Either::Left(name),
                        false => itertools::Either::Right(name),
                    }
                } else {
                    itertools::Either::Left(name)
                }
            });

        // If a plugin allows for overriding enablement, then it also allows
        // for enabling / disabling on `all`. Otherwise, just the presence of the
        // `all` key is enough to signal that all should be targeted.
        let all_enabled = if subgraph_can_override {
            prefixed_config
                .get("all")
                .and_then(|a| a.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_default()
        } else {
            prefixed_config.get("all").is_some()
        };

        // Now actually calculate which are incompatible
        // Note: We need to collect here because we need to know if the iterator
        // is empty or not when printing the warning message.
        let incompatible = if all_enabled {
            // If all are enabled, then we can subtract out those which are disabled explicitly
            connector_enabled_subgraphs
                .difference(&disabled)
                .copied()
                .collect::<HashSet<&String>>()
        } else {
            // Otherwise, then we only care about those explicitly enabled
            enabled
                .intersection(&connector_enabled_subgraphs)
                .copied()
                .collect::<HashSet<&String>>()
        };

        if !incompatible.is_empty() {
            tracing::warn!(
                subgraphs = incompatible.iter().join(","),
                message = msg(plugin)
            );
        }
    }

    // There are a few plugins which influence all subgraphs, regardless
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
