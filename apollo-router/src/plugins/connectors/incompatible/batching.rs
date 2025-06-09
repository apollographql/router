use itertools::Either;
use itertools::Itertools;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;
use crate::configuration::Batching;

pub(super) struct BatchingIncompatPlugin {
    config: Batching,
}

impl BatchingIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        // Batching is always default initialized, but can be explicitly
        // disabled by the config, so we init this plugin only if enabled.
        config.batching.enabled.then_some(Self {
            config: config.batching.clone(),
        })
    }
}

impl IncompatiblePlugin for BatchingIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // Batching allows for explicitly disabling it for all subgraphs,
        // with overrides optionally set at the subgraph level
        self.config
            .subgraph
            .as_ref()
            .map(|conf| conf.all.enabled)
            .unwrap_or_default()
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Subgraphs can expliciltly enable / disable batching, so we partition
        // here for those cases
        self.config
            .subgraph
            .as_ref()
            .map(|conf| {
                conf.subgraphs
                    .iter()
                    .partition_map(|(name, batch)| match batch.enabled {
                        true => Either::Left(name),
                        false => Either::Right(name),
                    })
            })
            .map(|(enabled, disabled)| ConfiguredSubgraphs { enabled, disabled })
            .unwrap_or_default()
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self
                .config
                .subgraph
                .as_ref()
                .map(|conf| conf.subgraphs.contains_key(subgraph))
                .unwrap_or_default()
            {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `batching` is explicitly configured for connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `batching` indirectly targets a connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
