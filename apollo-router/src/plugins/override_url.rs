use apollo_router_core::{register_plugin, Plugin, SubgraphRequest, SubgraphResponse};
use reqwest::Url;
use std::collections::HashMap;
use tower::util::BoxService;
use tower::{BoxError, ServiceExt};

#[derive(Debug)]
struct OverrideSubgraphUrl {
    urls: HashMap<String, Url>,
}

#[async_trait::async_trait]
impl Plugin for OverrideSubgraphUrl {
    type Config = HashMap<String, Url>;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("OverrideSubgraphUrl {:#?}!", configuration);
        Ok(OverrideSubgraphUrl {
            urls: configuration,
        })
    }

    fn subgraph_service(
        &mut self,
        subgraph_name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let mut new_url = self.urls.get(subgraph_name).cloned();
        service
            .map_request(move |mut req: SubgraphRequest| {
                if let Some(new_url) = new_url.take() {
                    req.http_request
                        .set_url(new_url)
                        .expect("url has been checked when we configured the plugin");
                }

                req
            })
            .boxed()
    }
}

register_plugin!(
    "com.apollographql",
    "override_subgraph_url",
    OverrideSubgraphUrl
);

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use std::str::FromStr;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("com.apollographql.override_subgraph_url")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(
                    r#"{
                "test_one": "http://localhost:8001",
                "test_two": "http://localhost:8002"
            }"#,
                )
                .unwrap(),
            )
            .unwrap();
    }
}
