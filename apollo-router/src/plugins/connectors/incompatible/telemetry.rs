use itertools::Either;
use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;
use crate::plugins::telemetry::apollo;

pub(super) struct TelemetryIncompatPlugin {
    config: apollo::SubgraphErrorConfig,
}

impl TelemetryIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("telemetry")
            .and_then(|val| val.get("apollo"))
            .and_then(|raw| serde_json::from_value(raw.clone()).ok())
            .map(|config: apollo::Config| TelemetryIncompatPlugin {
                config: config.errors.subgraph,
            })
    }
}

impl IncompatiblePlugin for TelemetryIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        self.config.all.send || self.config.all.redact
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // While you can't necessarily disable telemetry errors per subgraph,
        // you can technically disable doing anything with it.
        let (enabled, disabled) =
            self.config.subgraphs.iter().partition_map(|(name, sub)| {
                match sub.send || sub.redact {
                    true => Either::Left(name),
                    false => Either::Right(name),
                }
            });

        super::ConfiguredSubgraphs { enabled, disabled }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::sources::connect::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `telemetry` is explicitly configured to send errors to Apollo studio for connector-enabled subgraph, which is not supported",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `telemetry` is indirectly configured to send errors to Apollo studio for a connector-enabled subgraph, which is not supported",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
