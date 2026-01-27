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
use crate::services::SubgraphRequest;
use crate::services::subgraph;

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
                    // WARN: this allows for both relative paths, unix://relative/path.sock, and
                    // absolute paths, unix:///absolute/path.sock, and we _could_ add validation to
                    // make sure that the path is absolute, but since this is out in the wild, we
                    // can't safely do that without potentially breaking it for someone. If you're
                    // trying to figure out why a socket is returning a bunch of connection errors,
                    // this might be why. In implementing coprocessor uds support, we make sure
                    // that the paths are absolute
                    if let Some(url_path) = url.strip_prefix("unix://") {
                        // there is no specified format for unix socket URLs (cf https://github.com/whatwg/url/issues/577)
                        // so a unix:// URL will not be parsed by http::Uri
                        // To fix that, hyperlocal came up with its own Uri type that can be converted to http::Uri.
                        // It hides the socket path in a hex encoded authority that the unix socket connector will
                        // know how to decode
                        //
                        // supports optional `path` query parameter for downstream HTTP paths (eg,
                        // when using a coprocessor). That looks like this: unix:///tmp/socket.sock?path=/api/v1
                        let (socket_path, http_path) = if let Some(query_start) = url_path.find('?')
                        {
                            let socket = &url_path[..query_start];
                            let query = &url_path[query_start + 1..];
                            let path = query
                                .split('&')
                                .find_map(|param| param.strip_prefix("path="))
                                .unwrap_or("/");
                            (socket, path)
                        } else {
                            (url_path, "/")
                        };
                        Ok((k, hyperlocal::Uri::new(socket_path, http_path).into()))
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
    use tower::Service;
    use tower::ServiceExt;
    use tower::util::BoxService;

    use crate::Context;
    use crate::plugin::DynPlugin;
    use crate::plugin::test::MockSubgraphService;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;

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
