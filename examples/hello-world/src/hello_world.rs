use apollo_router::plugin::Plugin;
use apollo_router::{
    register_plugin, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, Response, ResponseBody, RouterRequest, RouterResponse, SubgraphRequest,
    SubgraphResponse,
};
use futures::stream::BoxStream;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

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

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(HelloWorld { configuration })
    }

    fn router_service(
        &mut self,
        service: BoxService<
            RouterRequest,
            RouterResponse<BoxStream<'static, ResponseBody>>,
            BoxError,
        >,
    ) -> BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError> {
        // Say hello when our service is added to the router_service
        // stage of the router plugin pipeline.
        #[cfg(test)]
        println!("Hello {}", self.configuration.name);
        #[cfg(not(test))]
        tracing::info!("Hello {}", self.configuration.name);
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

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        // This is the default implementation and does not modify the default service.
        // The trait also has this implementation, and we just provide it here for illustration.
        service
    }

    fn execution_service(
        &mut self,
        service: BoxService<
            ExecutionRequest,
            ExecutionResponse<BoxStream<'static, Response>>,
            BoxError,
        >,
    ) -> BoxService<ExecutionRequest, ExecutionResponse<BoxStream<'static, Response>>, BoxError>
    {
        //This is the default implementation and does not modify the default service.
        // The trait also has this implementation, and we just provide it here for illustration.
        service
    }

    // Called for each subgraph
    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
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

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "hello_world", HelloWorld);

#[cfg(test)]
mod tests {
    use super::{Conf, HelloWorld};

    use apollo_router::utils::test::IntoSchema::Canned;
    use apollo_router::utils::test::PluginTestHarness;
    use apollo_router::Plugin;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router::plugins()
            .get("example.hello_world")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({"name" : "Bob"}))
            .await
            .unwrap();
    }

    // If we run this test as follows: cargo test -- --nocapture
    // we will see the message "Hello Bob" printed to standard out
    #[tokio::test]
    async fn display_message() {
        // Define a configuration to use with our plugin
        let conf = Conf {
            name: "Bob".to_string(),
        };

        // Build an instance of our plugin to use in the test harness
        let plugin = HelloWorld::new(conf).await.expect("created plugin");

        // Build a test harness. Usually we'd use this and send requests to
        // it, but in this case it's enough to build the harness to see our
        // output when our service registers.
        let _test_harness = PluginTestHarness::builder()
            .plugin(plugin)
            .schema(Canned)
            .build()
            .await
            .expect("building harness");
    }
}
