use std::collections::HashSet;

use apollo_federation::connectors::expand::Connectors;
use apq::APQIncompatPlugin;
use authentication::AuthIncompatPlugin;
use batching::BatchingIncompatPlugin;
use coprocessor::CoprocessorIncompatPlugin;
use entity_cache::EntityCacheIncompatPlugin;
use headers::HeadersIncompatPlugin;
use rhai::RhaiIncompatPlugin;
use telemetry::TelemetryIncompatPlugin;
use tls::TlsIncompatPlugin;
use traffic_shaping::TrafficShapingIncompatPlugin;
use url_override::UrlOverrideIncompatPlugin;

use crate::Configuration;

mod apq;
mod authentication;
mod batching;
mod coprocessor;
mod entity_cache;
mod headers;
mod rhai;
mod telemetry;
mod tls;
mod traffic_shaping;
mod url_override;

/// Pair of explicitly configured subgraphs for a plugin
#[derive(Default)]
struct ConfiguredSubgraphs<'a> {
    /// Subgraphs which are explicitly enabled
    enabled: HashSet<&'a String>,

    /// Subgraphs that are explicitly disabled
    /// Note: Not all plugins allow for explicitly disabling a subgraph
    disabled: HashSet<&'a String>,
}

/// Trait describing a connector-enabled subgraph incompatible plugin
///
/// Certain features of the router are not currently compatible with subgraphs
/// which use connectors, so those plugins can mark themselves as incompatible
/// with this trait.
///
/// Note: Care should be taken to not spam the end-user with warnings that
/// either cannot be resolved or are not applicable in all circumstances.
trait IncompatiblePlugin {
    /// Whether the plugin is currently configured to apply to all subgraphs
    fn is_applied_to_all(&self) -> bool;

    /// Get all explicitly configured subgraphs for this plugin
    fn configured_subgraphs(&self) -> ConfiguredSubgraphs<'_>;

    /// Inform the user of incompatibilities with provided subgraphs
    fn inform_incompatibilities(&self, subgraphs: HashSet<&String>, connectors: &Connectors);
}

/// Warn about possible incompatibilities with other router features / plugins.
///
/// Connectors do not currently work with some of the existing router
/// features, so we need to inform the user when those features are
/// detected as being enabled.
pub(crate) fn warn_incompatible_plugins(config: &Configuration, connectors: &Connectors) {
    let connector_enabled_subgraphs: HashSet<&String> = connectors
        .by_service_name
        .values()
        .map(|v| &v.id.subgraph_name)
        .collect();

    // If we don't have any connector-enabled subgraphs, then no need to warn
    if connector_enabled_subgraphs.is_empty() {
        return;
    }

    // Specify all of the incompatible plugin handlers that should warn
    //
    // Note: Plugin configuration is only populated if the user has specified it,
    // so we can skip any that are missing.
    macro_rules! boxify {
        () => {
            |a| {
                let boxed: Box<dyn IncompatiblePlugin> = Box::new(a);
                boxed
            }
        };
    }
    let incompatible_plugins: Vec<Box<dyn IncompatiblePlugin>> = vec![
        APQIncompatPlugin::from_config(config).map(boxify!()),
        AuthIncompatPlugin::from_config(config).map(boxify!()),
        BatchingIncompatPlugin::from_config(config).map(boxify!()),
        CoprocessorIncompatPlugin::from_config(config).map(boxify!()),
        EntityCacheIncompatPlugin::from_config(config).map(boxify!()),
        HeadersIncompatPlugin::from_config(config).map(boxify!()),
        RhaiIncompatPlugin::from_config(config).map(boxify!()),
        TelemetryIncompatPlugin::from_config(config).map(boxify!()),
        TlsIncompatPlugin::from_config(config).map(boxify!()),
        TrafficShapingIncompatPlugin::from_config(config).map(boxify!()),
        UrlOverrideIncompatPlugin::from_config(config).map(boxify!()),
    ]
    .into_iter()
    .flatten()
    .collect();

    for plugin in incompatible_plugins {
        // Grab all of the configured subgraphs for this plugin
        let ConfiguredSubgraphs { enabled, disabled } = plugin.configured_subgraphs();

        // Now actually calculate which are incompatible
        // Note: We need to collect here because we need to know if the iterator
        // is empty or not when printing the warning message.
        let incompatible = if plugin.is_applied_to_all() {
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

        // Now warn for each subgraph that is targeted by the incompatible plugin
        if !incompatible.is_empty() {
            plugin.inform_incompatibilities(incompatible, connectors);
        }
    }
}
