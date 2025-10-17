use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;

pub(super) struct RhaiIncompatPlugin;

impl RhaiIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("rhai")
            .map(|_| RhaiIncompatPlugin)
    }
}

impl IncompatiblePlugin for RhaiIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // Rhai is always applied to all subgraphs since it modifies
        // the lifecycle of each router request
        true
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Rhai cannot be configured at the subgraph level
        Default::default()
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        tracing::info!(
            subgraphs = subgraphs.into_iter().join(","),
            message = "rhai scripts which hook into `subgraph_request` or `subgraph_response` won't be used by connector-enabled subgraphs",
            see = "https://go.apollo.dev/connectors/incompat",
        );
    }
}
