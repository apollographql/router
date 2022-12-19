use std::net::SocketAddr;

use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::router;
use apollo_router::Endpoint;
use apollo_router::ListenAddr;
use futures::future::BoxFuture;
use http::StatusCode;
use hyper::body::Bytes;
use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

#[derive(Debug)]
struct EchoCoProcessor {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    port: u16, // The port the custom echo server will listen to
}

// This is a bare bones plugin that can be duplicated when creating your own.
#[async_trait::async_trait]
impl Plugin for EchoCoProcessor {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            configuration: init.config,
        })
    }

    // This dummy endpoint will listen to the port defined in the yml,
    // dump the received payload and return it as is
    // In real life, the coprossor will be on an other web server
    // written in the language you're comfortable with.
    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let web_endpoint = Endpoint::from_router_service("/".to_string(), EchoServer {}.boxed());

        let mut endpoints = MultiMap::new();
        let socket_addr: SocketAddr = format!("127.0.0.1:{}", self.configuration.port)
            .parse()
            .unwrap();
        endpoints.insert(ListenAddr::from(socket_addr), web_endpoint);

        endpoints
    }
}

// This is a dummy echo server, that will dump the payload and return it.
// In real life you will implement this outside the router, in your favorite language.
struct EchoServer {}

impl Service<router::Request> for EchoServer {
    type Response = router::Response;

    type Error = BoxError;

    type Future = BoxFuture<'static, router::ServiceResult>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        tracing::info!("received request");
        tracing::info!("JSON context:");
        tracing::info!("{}", serde_json::to_string_pretty(&req.context).unwrap());

        let fut = async move {
            let body = req.router_request.into_body();

            let body = hyper::body::to_bytes(body).await.unwrap();

            let mut json_body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            tracing::info!("got payload:");
            tracing::info!("{}", serde_json::to_string_pretty(&json_body).unwrap());

            // let's add an arbitrary header to the request
            if let Some(headers) = json_body.get_mut("headers") {
                headers.as_object_mut().map(|headers| {
                    headers.insert(
                        "x-my-subgraph-api-key".to_string(),
                        json! {["ThisIsATestApiKey"]}, // header values are arrays
                    );
                });
            } else {
                json_body.as_object_mut().map(|body| {
                    body.insert(
                        "headers".to_string(),
                        json! {{
                            "x-my-subgraph-api-key": ["ThisIsATestApiKey"] // header values are arrays
                        }},
                    )
                });
            };

            tracing::info!("modified payload:");
            tracing::info!("{}", serde_json::to_string_pretty(&json_body).unwrap());

            // return the modified payload
            let http_response = http::Response::builder()
                .status(StatusCode::OK)
                .body(hyper::Body::from(Bytes::from(
                    serde_json::to_vec(&json_body).unwrap(),
                )))
                .unwrap();
            let mut router_response = router::Response::from(http_response);
            router_response.context = req.context;

            Ok(router_response)
        };

        Box::pin(fut)
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "echo_co_processor", EchoCoProcessor);

#[cfg(test)]
mod tests {
    // If we run this test as follows: cargo test -- --nocapture
    // we will see the message "Hello Bob" printed to standard out
    #[tokio::test]
    async fn display_message() {
        let config = serde_json::json!({
            "plugins": {
                "example.echo_co_processor": {
                    "port": 8080
                }
            }
        });
        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }
}
