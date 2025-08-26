use itertools::Either;
use itertools::Itertools as _;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;
use crate::configuration::Apq;

pub(super) struct APQIncompatPlugin {
    config: Apq,
}

impl APQIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        // Apq is always default initialized, but can be explicitly
        // disabled by the config, so we init this plugin only if enabled.
        config.apq.enabled.then_some(Self {
            config: config.apq.clone(),
        })
    }
}

impl IncompatiblePlugin for APQIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // Aqp allows for explicitly disabling it for all subgraphs,
        // with overrides optionally set at the subgraph level
        self.config.subgraph.all.enabled
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Subgraphs can expliciltly enable / disable aqp, so we partition
        // here for those cases
        let (enabled, disabled) =
            self.config
                .subgraph
                .subgraphs
                .iter()
                .partition_map(|(name, conf)| match conf.enabled {
                    true => Either::Left(name),
                    false => Either::Right(name),
                });

        ConfiguredSubgraphs { enabled, disabled }
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
                    message = "plugin `apq` is explicitly configured for connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `apq` indirectly targets a connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
