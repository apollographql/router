use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::supergraph;
{{#if type_basic}}
use apollo_router::services::router;
use apollo_router::services::execution;
use apollo_router::services::subgraph;
{{/if}}
{{#if type_auth}}
use apollo_router::layers::ServiceBuilderExt;
use std::ops::ControlFlow;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
{{#if type_tracing}}
use apollo_router::layers::ServiceBuilderExt;
use tower::ServiceExt;
use tower::ServiceBuilder;
{{/if}}
use schemars::JsonSchema;
use serde::Deserialize;
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

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::info!("{}", init.config.message);
        Ok({{pascal_name}} { configuration: init.config })
    }

    // Delete this function if you are not customizing it.
    fn router_service(
        &self,
        service: router::BoxService,
    ) -> router::BoxService {
        // Always use service builder to compose your plugins.
        // It provides off the shelf building blocks for your plugin.
        //
        // ServiceBuilder::new()
        //             .service(service)
        //             .boxed()

        // Returning the original service means that we didn't add any extra functionality at this point in the lifecycle.
        service
    }

    // Delete this function if you are not customizing it.
    fn supergraph_service(
        &self,
        service: supergraph::BoxService,
    ) -> supergraph::BoxService {
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
    fn execution_service(
        &self,
        service: execution::BoxService,
    ) -> execution::BoxService {
        service
    }

    // Delete this function if you are not customizing it.
    fn subgraph_service(
        &self,
        _name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        service
    }
}
{{/if}}
{{#if type_auth}}
// This plugin is a skeleton for doing authentication that requires a remote call.
#[async_trait::async_trait]
impl Plugin for {{pascal_name}} {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::info!("{}", init.config.message);
        Ok({{pascal_name}} { configuration: init.config })
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxService,
    ) -> supergraph::BoxService {

        ServiceBuilder::new()
                    .oneshot_checkpoint_async(|request : supergraph::Request| async {
                        // Do some async call here to auth, and decide if to continue or not.
                        Ok(ControlFlow::Continue(request))
                    })
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

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::info!("{}", init.config.message);
        Ok({{pascal_name}} { configuration: init.config })
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxService,
    ) -> supergraph::BoxService {

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
    use apollo_router::TestHarness;
    use apollo_router::services::supergraph;
    use apollo_router::graphql;
    use tower::BoxError;
    use tower::ServiceExt;

    #[tokio::test]
    async fn basic_test() -> Result<(), BoxError> {
        let test_harness = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "plugins": {
                    "{{project_name}}.{{snake_name}}": {
                        "message" : "Starting my plugin"
                    }
                }
            }))
            .unwrap()
            .build_router()
            .await
            .unwrap();
        let request = supergraph::Request::canned_builder().build().unwrap();
        let mut streamed_response = test_harness.oneshot(request.try_into()?).await?;

        let first_response: graphql::Response =
            serde_json::from_slice(streamed_response
                .next_response()
                .await
                .expect("couldn't get primary response")?.to_vec().as_slice()).unwrap();

        assert!(first_response.data.is_some());

        println!("first response: {:?}", first_response);
        let next = streamed_response.next_response().await;
        println!("next response: {:?}", next);

        // You could keep calling .next_response() until it yields None if you're expexting more parts.
        assert!(next.is_none());
        Ok(())
    }
}

