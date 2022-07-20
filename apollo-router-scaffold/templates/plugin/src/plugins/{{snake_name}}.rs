use apollo_router::plugin::Plugin;
use apollo_router::register_plugin;
{{#if type_basic}}
use apollo_router::services::{ExecutionRequest, ExecutionResponse};
use apollo_router::services::{QueryPlannerRequest, QueryPlannerResponse};
use apollo_router::services::{RouterRequest, RouterResponse};
use apollo_router::services::{SubgraphRequest, SubgraphResponse};
{{/if}}
{{#if type_auth}}
use apollo_router::services::{RouterRequest, RouterResponse};
use apollo_router::layers::ServiceBuilderExt;
use std::ops::ControlFlow;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
{{#if type_tracing}}
use apollo_router::services::{RouterRequest, RouterResponse};
use apollo_router::layers::ServiceBuilderExt;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
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
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
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
        &self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        service
    }

    // Delete this function if you are not customizing it.
    fn execution_service(
        &self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        service
    }

    // Delete this function if you are not customizing it.
    fn subgraph_service(
        &self,
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
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {

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
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {

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
    use apollo_router::plugin::Plugin;
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

        assert!(first_response.data.is_some());

        // You could keep calling result.next_response() until it yields None if you're expexting more parts.
        assert!(result.next_response().await.is_none());
        Ok(())
    }
}

