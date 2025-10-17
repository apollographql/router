use std::collections::HashMap;
use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;

#[derive(Debug, Deserialize)]
struct Config {
    all: Option<Value>,
    subgraphs: Option<HashMap<String, Value>>,
}

pub(super) struct TrafficShapingIncompatPlugin {
    config: Config,
}

impl TrafficShapingIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        config
            .apollo_plugins
            .plugins
            .get("traffic_shaping")
            .and_then(|raw| serde_json::from_value(raw.clone()).ok())
            .map(|config| Self { config })
    }
}

impl IncompatiblePlugin for TrafficShapingIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        self.config.all.is_some()
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // Apq does not support manually marking subgraphs as
        // disabled, so any subgraph listed is enabled.
        ConfiguredSubgraphs {
            enabled: self
                .config
                .subgraphs
                .as_ref()
                .map(|subs| subs.keys().collect())
                .unwrap_or_default(),
            disabled: HashSet::with_hasher(Default::default()),
        }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self
                .config
                .subgraphs
                .as_ref()
                .map(|subs| subs.contains_key(subgraph))
                .unwrap_or_default()
            {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "The `traffic_shaping` plugin is explicitly configured for a subgraph containing connectors, which is not supported. Instead, configure the connector sources directly using `traffic_shaping.connector.sources.<subgraph_name>.<source_name>`.",
                    see = "https://www.apollographql.com/docs/graphos/schema-design/connectors/router#traffic-shaping",
                );
            }
        }
    }
}
