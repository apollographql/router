use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;
use crate::plugins::coprocessor;

pub(super) struct CoprocessorIncompatPlugin;

impl CoprocessorIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("coprocessor")
            .and_then(|val| val.get("subgraph"))
            .and_then(|val| val.get("all"))
            .and_then(|raw| serde_json::from_value(raw.clone()).ok())
            .map(|_: coprocessor::SubgraphStage| CoprocessorIncompatPlugin)
    }
}

impl IncompatiblePlugin for CoprocessorIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // If the coprocessor is configured with a subgraph setting, then it is
        // for sure applied to all as there is no other configuration available
        true
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Coprocessors cannot be configured at the subgraph level
        Default::default()
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        tracing::info!(
            subgraphs = subgraphs.into_iter().join(","),
            message = "coprocessors which hook into `subgraph_request` or `subgraph_response` won't be used by connector-enabled subgraphs",
            see = "https://go.apollo.dev/connectors/incompat",
        );
    }
}
