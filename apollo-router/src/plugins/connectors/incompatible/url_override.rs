use std::collections::HashSet;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;

pub(super) struct UrlOverrideIncompatPlugin {
    configured: HashSet<String>,
}

impl UrlOverrideIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("override_subgraph_url")
            .and_then(serde_json::Value::as_object)
            .map(|configured| UrlOverrideIncompatPlugin {
                configured: configured.keys().cloned().collect(),
            })
    }
}

impl IncompatiblePlugin for UrlOverrideIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // Overrides are per subgraph, so it can never target all
        false
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Overrides cannot be explicitly disabled, so all present overrides
        // are always enabled
        ConfiguredSubgraphs {
            enabled: self.configured.iter().by_ref().collect(),
            disabled: HashSet::with_hasher(Default::default()),
        }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            tracing::warn!(
                subgraph = subgraph,
                message =
                    "overriding a subgraph URL for a connectors-enabled subgraph is not supported",
                see = "https://go.apollo.dev/connectors/incompat",
            );
        }
    }
}
