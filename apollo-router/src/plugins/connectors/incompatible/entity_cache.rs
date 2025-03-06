use itertools::Either;
use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;
use crate::plugins::cache::entity;

pub(super) struct EntityCacheIncompatPlugin {
    config: entity::Config,
}

impl EntityCacheIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("preview_entity_cache")
            .and_then(|raw| serde_json::from_value(raw.clone()).ok())
            .and_then(|config: entity::Config| {
                config
                    .enabled
                    .then_some(EntityCacheIncompatPlugin { config })
            })
    }
}

impl IncompatiblePlugin for EntityCacheIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        self.config.subgraph.all.enabled
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        let (enabled, disabled) =
            self.config
                .subgraph
                .subgraphs
                .iter()
                .partition_map(|(name, sub)| match sub.enabled {
                    true => Either::Left(name),
                    false => Either::Right(name),
                });

        super::ConfiguredSubgraphs { enabled, disabled }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::sources::connect::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraph.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `preview_entity_cache` is explicitly configured for connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `preview_entity_cache` indirectly targets a connector-enabled subgraph, which is not supported.",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
