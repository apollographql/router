use apollo_router_core::plugin::Plugin;
use apollo_router_core::{
    register_plugin, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
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
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
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
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
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

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("example.hello_world")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({"name" : "Bob"}))
            .await
            .unwrap();
    }
}
