use itertools::Either;
use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;
use crate::plugins::telemetry::apollo;
use crate::plugins::telemetry::config::Conf;

pub(super) struct TelemetryIncompatPlugin {
    config: apollo::ErrorsConfiguration,
}

impl TelemetryIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        Some(TelemetryIncompatPlugin {
            config: Conf::apollo(config).errors,
        })
    }
}

impl IncompatiblePlugin for TelemetryIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        self.config.subgraph.all.send
            // When ExtendedErrorMetricsMode is enabled, this plugin supports reporting connector errors
            && !matches!(
                self.config.preview_extended_error_metrics,
                apollo::ExtendedErrorMetricsMode::Enabled
            )
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // While you can't necessarily disable telemetry errors per subgraph,
        // you can technically disable doing anything with it.
        let (enabled, disabled) =
            self.config
                .subgraph
                .subgraphs
                .iter()
                .partition_map(|(name, sub)| {
                    if sub.send
                        && !matches!(
                            self.config.preview_extended_error_metrics,
                            apollo::ExtendedErrorMetricsMode::Enabled
                        )
                    {
                        Either::Left(name)
                    } else {
                        Either::Right(name)
                    }
                });

        super::ConfiguredSubgraphs { enabled, disabled }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraph.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `telemetry` is explicitly configured to send errors to Apollo studio for connector-enabled subgraph, which is only supported when `preview_extended_error_metrics` is enabled",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `telemetry` is indirectly configured to send errors to Apollo studio for a connector-enabled subgraph, which is only supported when `preview_extended_error_metrics` is enabled",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
