use std::collections::HashMap;
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::subgraph;

register_private_plugin!("apollo", "mock_subgraphs", MockSubgraphsPlugin);

/// Configuration for the `mock_subgraphs` plugin
type Config = HashMap<String, Arc<SubgraphConfig>>;

/// Configuration for one subgraph for the `mock_subgraphs` plugin
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SubgraphConfig {}

struct MockSubgraphsPlugin {
    _per_subgraph_config: Config,
}

#[async_trait::async_trait]
impl PluginPrivate for MockSubgraphsPlugin {
    type Config = Config;

    const HIDDEN_FROM_CONFIG_JSON_SCHEMA: bool = true;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            _per_subgraph_config: init.config,
        })
    }

    fn subgraph_service(&self, _name: &str, _: subgraph::BoxService) -> subgraph::BoxService {
        tower::service_fn(move |request: subgraph::Request| async move {
            let body = graphql::Response::builder()
                .error(
                    graphql::Error::builder()
                        .message("subgraph mock not configured")
                        .extension_code("SUBGRAPH_MOCK_NOT_CONFIGURED")
                        .build(),
                )
                .build();
            let response = http::Response::builder().body(body).unwrap();
            Ok(subgraph::Response::new_from_response(
                response,
                request.context,
                request.subgraph_name,
                request.id,
            ))
        })
        .boxed()
    }
}
