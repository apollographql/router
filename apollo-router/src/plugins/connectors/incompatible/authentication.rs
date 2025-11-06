use std::collections::HashSet;

use apollo_federation::connectors::expand::Connectors;
use itertools::Itertools;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;
use crate::plugins::authentication;
use crate::plugins::authentication::connector;

/// Incompatibility handler for the built-in authentication plugin
pub(super) struct AuthIncompatPlugin {
    // Auth configuration per subgraph
    subgraph: authentication::subgraph::Config,

    /// The auth configuration per connector source
    /// Note: We don't necessarily care about how each source is configured,
    /// only that it has an entry in the config.
    connector_sources: Option<connector::Config>,
}

impl AuthIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        let plugin_config = config.apollo_plugins.plugins.get("authentication");
        let subgraph_config = plugin_config
            .and_then(|plugin| plugin.get("subgraph"))
            .and_then(|subgraph_config| serde_json::from_value(subgraph_config.clone()).ok());
        let connector_sources = plugin_config
            .and_then(|plugin| plugin.get("connector"))
            .and_then(|sources| serde_json::from_value(sources.clone()).ok());

        subgraph_config.map(|subgraph| AuthIncompatPlugin {
            subgraph,
            connector_sources,
        })
    }
}

impl IncompatiblePlugin for AuthIncompatPlugin {
    fn configured_subgraphs(&self) -> ConfiguredSubgraphs<'_> {
        // Authentication does not support manually marking subgraphs as
        // disabled, so any subgraph listed is enabled.
        ConfiguredSubgraphs {
            enabled: self.subgraph.subgraphs.keys().collect(),
            disabled: HashSet::with_hasher(Default::default()),
        }
    }

    fn is_applied_to_all(&self) -> bool {
        self.subgraph.all.is_some()
    }

    fn inform_incompatibilities(&self, subgraphs: HashSet<&String>, connectors: &Connectors) {
        // If the user has not configured any connector-related options on authentication,
        // then all passed subgraphs are misconfigured.
        let Some(connector_sources) = self.connector_sources.as_ref() else {
            return tracing::warn!(
                subgraphs = subgraphs.iter().join(","),
                message = "plugin `authentication` is enabled for connector-enabled subgraphs, which requires a different configuration to work properly",
                see = "https://www.apollographql.com/docs/graphos/schema-design/connectors/router#authentication",
            );
        };

        // Authentication is technically compatible with connectors, but it must be configured
        // at the connector source level rather than subgraph level. Here we collect
        // all subgraphs and their set of sources.
        //
        // Note: Named sources are optional for connectors, so any connector that does not have
        // one is misconfigured by default for authentication.
        let sources = connectors
            .by_service_name
            .values()
            .filter(|connector| {
                subgraphs.contains(&connector.id.subgraph_name)
                    && !connector
                        .id
                        .source_name
                        .as_ref()
                        .map(|src| {
                            connector_sources
                                .sources
                                .contains_key(&format!("{}.{src}", connector.id.subgraph_name))
                        })
                        .unwrap_or(false)
            })
            .map(|connector| {
                (
                    connector.id.subgraph_name.as_str(),
                    connector
                        .id
                        .source_name
                        .as_ref()
                        .map(|name| name.to_string())
                        .unwrap_or(format!(
                            "<anonymous source for {}>",
                            connector.label.as_ref()
                        )),
                )
            })
            .into_grouping_map()
            .collect::<HashSet<String>>();

        // Verify that for every affected subgraph that its sources have been configured separately
        for (subgraph, srcs) in sources {
            tracing::warn!(
                subgraph = subgraph,
                sources = srcs.into_iter().join(","),
                message = "plugin `authentication` is enabled for a connector-enabled subgraph, which requires a different configuration to work properly",
                see = "https://www.apollographql.com/docs/graphos/schema-design/connectors/router#authentication",
            );
        }
    }
}
