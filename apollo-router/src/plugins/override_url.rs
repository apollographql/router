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
        println!("Request for subgraph '{subgraph_name}'");
        service
            .map_request(move |mut req: SubgraphRequest| {
                println!("  with URL '{}'", req.http_request.inner.uri());

                if let Some(new_url) = new_url.take() {
                    println!("  replaced by '{}'", new_url);

                    req.http_request
                        .set_url(new_url)
                        .expect("url has been checked when we configured the plugin");
                }

                req
            })
            .boxed()
    }
}

register_plugin!("override_subgraph_url", OverrideSubgraphUrl);

#[cfg(test)]
mod tests {
    use apollo_router_core::{
        plugin_utils::{self, MockSubgraphService},
        Context, DynPlugin, SubgraphRequest,
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
                assert_eq!(
                    req.http_request.inner.uri(),
                    &Uri::from_static("http://localhost:8001")
                );
                true
            })
            .times(1)
            .returning(move |req: SubgraphRequest| {
                Ok(plugin_utils::SubgraphResponse::builder()
                    .context(req.context)
                    .build()
                    .into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("override_subgraph_url")
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
        let mut subgraph_service =
            dyn_plugin.subgraph_service("test_one", BoxService::new(mock_service.build()));
        let context = Context::new();
        context
            .insert_extension("test".to_string(), 5i64.into())
            .await;
        let subgraph_req = plugin_utils::SubgraphRequest::builder().context(context);

        let _subgraph_resp = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req.build().into())
            .await
            .unwrap();
    }
}
