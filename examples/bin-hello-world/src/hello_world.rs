use apollo_router::declare_plugin;
use apollo_router::plugin::DynPlugin;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::services::execution;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use async_ffi::FfiFuture;
use async_ffi::FutureExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct HelloWorld {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Conf {
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

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        // Say hello when our service is added to the router_service
        // stage of the router plugin pipeline.
        #[cfg(test)]
        println!("Binary Plugin Hello {}", self.configuration.name);
        #[cfg(not(test))]
        tracing::info!("Binary Plugin Hello {}", self.configuration.name);
        // Always use service builder to compose your plugins.
        // It provides off the shelf building blocks for your plugin.
        ServiceBuilder::new()
            // .map_request()
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        //This is the default implementation and does not modify the default service.
        // The trait also has this implementation, and we just provide it here for illustration.
        service
    }

    // Called for each subgraph
    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        // Always use service builder to compose your plugins.
        // It provides off the shelf building blocks for your plugin.
        ServiceBuilder::new()
            // .map_request()
            // .map_response()
            // .rate_limit()
            // .checkpoint()
            // .timeout()
            .service(service)
            .boxed()
    }
}

declare_plugin!(HelloWorld::new, Conf);

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
            .build()
            .await
            .unwrap();
    }
}
