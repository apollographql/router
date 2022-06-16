use apollo_router::plugin::Plugin;
{{#if type_basic}}
use apollo_router::{
    register_plugin, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, ResponseBody, RouterRequest, RouterResponse, Response, SubgraphRequest, SubgraphResponse,
};
{{/if}}
{{#if type_auth}}
use apollo_router::{
    register_plugin, ResponseBody, RouterRequest, RouterResponse,
};
use std::ops::ControlFlow;
use apollo_router::layers::ServiceBuilderExt;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
{{#if type_tracing}}
use apollo_router::{
    register_plugin, ResponseBody, RouterRequest, RouterResponse,
};
use apollo_router::layers::ServiceBuilderExt;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
use futures::stream::BoxStream;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;

#[derive(Debug)]
struct {{pascal_name}} {
    #[allow(dead_code)]
    configuration: Conf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Put your plugin configuration here. It will automatically be deserialized from JSON.
    // Always put some sort of config here, even if it is just a bool to say that the plugin is enabled,
    // otherwise the yaml to enable the plugin will be confusing.
    message: String,
}
{{#if type_basic}}
// This is a bare bones plugin that can be duplicated when creating your own.
#[async_trait::async_trait]
impl Plugin for {{pascal_name}} {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("{}", configuration.message);
        Ok({{pascal_name}} { configuration })
    }

    // Delete this function if you are not customizing it.
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError> {
        // Always use service builder to compose your plugins.
        // It provides off the shelf building blocks for your plugin.
        //
        // ServiceBuilder::new()
        //             .service(service)
        //             .boxed()

        // Returning the original service means that we didn't add any extra functionality for at this point in the lifecycle.
        service
    }

    // Delete this function if you are not customizing it.
    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        service
    }

    // Delete this function if you are not customizing it.
    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse<BoxStream<'static, Response>>, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse<BoxStream<'static, Response>>, BoxError> {
        service
    }

    // Delete this function if you are not customizing it.
    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        service
    }
}
{{/if}}
{{#if type_auth}}
// This plugin is a skeleton for doing authentication that requires a remote call.
#[async_trait::async_trait]
impl Plugin for {{pascal_name}} {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("{}", configuration.message);
        Ok({{pascal_name}} { configuration })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError> {

        ServiceBuilder::new()
                    .checkpoint_async(|request : RouterRequest| async {
                        // Do some async call here to auth, and decide if to continue or not.
                        Ok(ControlFlow::Continue(request))
                    })
                    .buffered()
                    .service(service)
                    .boxed()
    }
}
{{/if}}
{{#if type_tracing}}
// This plugin adds a span and an error to the logs.
#[async_trait::async_trait]
impl Plugin for {{pascal_name}} {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("{}", configuration.message);
        Ok({{pascal_name}} { configuration })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse<BoxStream<'static, ResponseBody>>, BoxError> {

        ServiceBuilder::new()
                    .instrument(|_request| {
                        // Optionally take information from the request and insert it into the span as attributes
                        // See https://docs.rs/tracing/latest/tracing/ for more information
                        tracing::info_span!("my_custom_span")
                    })
                    .map_request(|request| {
                        // Add a log message, this will appear within the context of the current span
                        tracing::error!("error detected");
                        request
                    })
                    .service(service)
                    .boxed()
    }
}
{{/if}}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
register_plugin!("{{project_name}}", "{{snake_name}}", {{pascal_name}});

#[cfg(test)]
mod tests {
    use super::{Conf, {{pascal_name}}};

    use apollo_router::plugin::test::IntoSchema::Canned;
    use apollo_router::plugin::test::PluginTestHarness;
    use apollo_router::{plugin::Plugin, ResponseBody};
    use tower::BoxError;

    #[tokio::test]
    async fn plugin_registered() {
        apollo_router::plugin::plugins()
            .get("{{project_name}}.{{snake_name}}")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({"message" : "Starting my plugin"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn basic_test() -> Result<(), BoxError> {
        // Define a configuration to use with our plugin
        let conf = Conf {
            message: "Starting my plugin".to_string(),
        };

        // Build an instance of our plugin to use in the test harness
        let plugin = {{pascal_name}}::new(conf).await.expect("created plugin");

        // Create the test harness. You can add mocks for individual services, or use prebuilt canned services.
        let mut test_harness = PluginTestHarness::builder()
            .plugin(plugin)
            .schema(Canned)
            .build()
            .await?;

        // Send a request
        let mut result = test_harness.call_canned().await?;

        let first_response = result
            .next_response()
            .await
            .expect("couldn't get primary response");

        if let ResponseBody::GraphQL(graphql) = first_response {
            assert!(graphql.data.is_some());
        } else {
            panic!("expected graphql response")
        }

        // You could keep calling result.next_response() until it yields None if you're expexting more parts.
        assert!(result.next_response().await.is_none());
        Ok(())
    }
}

