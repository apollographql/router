//! Allows subgraph URLs to be overridden.

use apollo_router_core::{register_plugin, Plugin, SubgraphRequest, SubgraphResponse};
use http::Uri;
use std::collections::HashMap;
use std::str::FromStr;
use tower::util::BoxService;
use tower::{BoxError, ServiceExt};

#[derive(Debug, Clone)]
struct OverrideSubgraphUrl {
    urls: HashMap<String, Uri>,
}

#[async_trait::async_trait]
impl Plugin for OverrideSubgraphUrl {
    type Config = HashMap<String, url::Url>;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(OverrideSubgraphUrl {
            urls: configuration
                .into_iter()
                .map(|(k, v)| (k, Uri::from_str(v.as_str()).unwrap()))
                .collect(),
        })
    }

    fn subgraph_service(
        &mut self,
        subgraph_name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let new_url = self.urls.get(subgraph_name).cloned();
        service
            .map_request(move |mut req: SubgraphRequest| {
                if let Some(new_url) = new_url.clone() {
                    *req.subgraph_request.uri_mut() = new_url;
                }

                req
            })
            .boxed()
    }
}

register_plugin!("apollo", "override_subgraph_url", OverrideSubgraphUrl);

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_router_core::{
        plugin::utils::test::MockSubgraphService, Context, DynPlugin, SubgraphRequest,
    };
    use http::Uri;
    use serde_json::Value;
    use std::str::FromStr;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn plugin_registered() {
        let mut mock_service = MockSubgraphService::new();
        mock_service
            .expect_call()
            .withf(|req| {
                req.subgraph_request.uri() == &Uri::from_str("http://localhost:8001").unwrap()
            })
            .times(1)
            .returning(move |req: SubgraphRequest| {
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("apollo.override_subgraph_url")
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
            .await
            .unwrap();
        let mut subgraph_service =
            dyn_plugin.subgraph_service("test_one", BoxService::new(mock_service.build()));
        let context = Context::new();
        context.insert("test".to_string(), 5i64).unwrap();
        let subgraph_req = SubgraphRequest::fake_builder().context(context);

        let _subgraph_resp = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req.build())
            .await
            .unwrap();
    }
}
