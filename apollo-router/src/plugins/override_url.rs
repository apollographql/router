//! Allows subgraph URLs to be overridden.

use std::collections::HashMap;
use std::str::FromStr;

use http::Uri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphRequest;

#[derive(Debug, Clone)]
struct OverrideSubgraphUrl {
    urls: HashMap<String, Uri>,
}

/// Subgraph URL mappings
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
enum Conf {
    /// Subgraph URL mappings
    Mapping(HashMap<String, String>),
}

#[async_trait::async_trait]
impl Plugin for OverrideSubgraphUrl {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let Conf::Mapping(urls) = init.config;
        Ok(OverrideSubgraphUrl {
            urls: urls
                .into_iter()
                .map(|(k, url)| {
                    #[cfg(unix)]
                    // there is no standard for unix socket URLs apparently
                    if let Some(path) = url.strip_prefix("unix://") {
                        // there is no specified format for unix socket URLs (cf https://github.com/whatwg/url/issues/577)
                        // so a unix:// URL will not be parsed by http::Uri
                        // To fix that, hyperlocal came up with its own Uri type that can be converted to http::Uri.
                        // It hides the socket path in a hex encoded authority that the unix socket connector will
                        // know how to decode
                        Ok((k, hyperlocal::Uri::new(path, "/").into()))
                    } else {
                        Uri::from_str(&url).map(|url| (k, url))
                    }
                    #[cfg(not(unix))]
                    Uri::from_str(&url).map(|url| (k, url))
                })
                .collect::<Result<_, _>>()?,
        })
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
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
    use std::str::FromStr;

    use http::Uri;
    use serde_json::Value;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::DynPlugin;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

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

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.override_subgraph_url")
            .expect("Plugin not found")
            .create_instance_without_schema(
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
            dyn_plugin.subgraph_service("test_one", BoxService::new(mock_service));
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
