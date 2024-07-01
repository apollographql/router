use std::collections::HashMap;

use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Transport;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::Configuration;

/// Connectors configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Connectors {
    /// Per subgraph configuration
    pub(crate) subgraphs: HashMap<String, HashMap<String, SourceApiConfiguration>>,
}

/// Configuration for a connector subgraph
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphConnectorConfiguration {
    /// Per API configuration
    pub(crate) apis: HashMap<String, SourceApiConfiguration>,
}

/// Configuration for a source API
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SourceApiConfiguration {
    /// Override the base URL for the API
    pub(crate) override_url: Option<Url>,
}

/// If a base URL override is specified for a source API, apply it to the connectors using that
/// source.
pub(crate) fn override_connector_base_urls<'a, I>(config: &Configuration, connectors: I)
where
    I: IntoIterator<Item = &'a mut Connector>,
{
    for connector in connectors {
        if let Some(url) = config
            .preview_connectors
            .subgraphs
            .get(&connector.id.subgraph_name.to_string())
            .and_then(|map| map.get(&connector.id.source_name.clone()?.to_string()))
            .and_then(|api_config| api_config.override_url.as_ref())
        {
            match &mut connector.transport {
                Transport::HttpJson(transport) => transport.base_url = url.to_string(),
            }
        }
    }
}
