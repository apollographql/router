use itertools::Itertools as _;

use super::IncompatiblePlugin;
use crate::Configuration;

pub(super) struct DemandControlIncompatPlugin;

impl DemandControlIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("demand_control")
            .and_then(|val| val.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .and_then(|enabled| enabled.then_some(DemandControlIncompatPlugin))
    }
}

impl IncompatiblePlugin for DemandControlIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        // Demand control applies to all subgraphs if enabled
        true
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Demand control cannot be configured per subgraph
        Default::default()
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::sources::connect::expand::Connectors,
    ) {
        tracing::warn!(
            subgraphs = subgraphs.into_iter().join(","),
            message = "demand control cost calculations do not take connector-enabled subgraphs into consideration",
            see = "https://go.apollo.dev/connectors/incompat",
        );
    }
}
