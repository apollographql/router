use apollo_federation::sources::connect::Connector;
use apollo_federation::sources::connect::Transport;

use super::plugin::ConnectorsConfig;
use crate::Configuration;

/// If a base URL override is specified for a source API, apply it to the connectors using that
/// source.
pub(crate) fn override_connector_base_urls<'a, I>(config: &Configuration, connectors: I)
where
    I: IntoIterator<Item = &'a mut Connector>,
{
    let Some(config) = config.apollo_plugins.plugins.get("preview_connectors") else {
        return;
    };
    let Ok(config) = serde_json::from_value::<ConnectorsConfig>(config.clone()) else {
        return;
    };

    for connector in connectors {
        if let Some(url) = config
            .subgraphs
            .get(&connector.id.subgraph_name)
            .and_then(|subgraph_config| {
                subgraph_config
                    .sources
                    .get(connector.id.source_name.as_ref()?)
            })
            .and_then(|api_config| api_config.override_url.as_ref())
        {
            match &mut connector.transport {
                Transport::HttpJson(transport) => transport.base_url = url.to_string(),
            }
        }
    }
}
