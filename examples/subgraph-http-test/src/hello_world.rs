use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::subgraph_http;
use reqwest::header::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use std::ops::ControlFlow;
use std::sync::Arc;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct HelloWorld {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    name: String, // The name of the entity you'd like to say hello to
}

// This is a bare bones plugin that can be duplicated when creating your own.
#[async_trait::async_trait]
impl Plugin for HelloWorld {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(HelloWorld {
            configuration: init.config,
        })
    }

    fn subgraph_http_service(
        &self,
        name: &str,
        service: subgraph_http::BoxService,
    ) -> subgraph_http::BoxService {
        // Say hello when our service is added to the router_service
        // stage of the router plugin pipeline.
        #[cfg(test)]
        println!("Hello {}", self.configuration.name);
        #[cfg(not(test))]
        tracing::info!("Hello {}", self.configuration.name);
        // Always use service builder to compose your plugins.
        // It provides off the shelf building blocks for your plugin.

        let name = Arc::new(name.to_string());
        ServiceBuilder::new()
            .checkpoint_async(move |mut req: subgraph_http::Request| {
                let name = name.clone();
                async move {
                    let my_ip = reqwest::Client::new()
                        .get("http://ipinfo.io/ip")
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();

                    tracing::info!("my ip is {} and the subgraph name is {}", &my_ip, &name);

                    req.subgraph_request.headers_mut().insert(
                        "x-server-ip",
                        HeaderValue::from_str(my_ip.as_str()).unwrap(),
                    );

                    req.subgraph_request.headers_mut().insert(
                        "x-subgraph-service-name",
                        HeaderValue::from_str(name.as_str()).unwrap(),
                    );

                    Ok(ControlFlow::Continue(req))
                }
            })
            .buffer(20_000)
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "hello_world", HelloWorld);

#[cfg(test)]
mod tests {
    // If we run this test as follows: cargo test -- --nocapture
    // we will see the message "Hello Bob" printed to standard out
    #[tokio::test]
    async fn display_message() {
        let config = serde_json::json!({
            "plugins": {
                "example.hello_world": {
                    "name": "Bob"
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
