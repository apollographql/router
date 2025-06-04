use std::collections::HashSet;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;
use crate::configuration::subgraph::SubgraphConfiguration;

pub(super) struct HeadersIncompatPlugin {
    /// Configured subgraphs for header propagation
    config: SubgraphConfiguration<Option<serde_json::Value>>,
}

impl HeadersIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("headers")
            .and_then(|headers| serde_json::from_value(headers.clone()).ok())
            .map(|subgraphs| HeadersIncompatPlugin { config: subgraphs })
    }
}

impl IncompatiblePlugin for HeadersIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        self.config.all.is_some()
    }

    fn configured_subgraphs(&self) -> ConfiguredSubgraphs<'_> {
        // Headers does not support manually marking subgraphs as
        // disabled, so any subgraph listed is enabled.
        ConfiguredSubgraphs {
            enabled: self.config.subgraphs.keys().collect(),
            disabled: HashSet::with_hasher(Default::default()),
        }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `headers` is explicitly configured for connector-enabled subgraph, which is not supported. Headers will not be applied",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `headers` indirectly targets a connector-enabled subgraph, which is not supported. Headers will not be applied",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
