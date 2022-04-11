use crate::configuration::{Configuration, ConfigurationError};
use apollo_router_core::{
    http_compat::{Request, Response},
    PluggableRouterServiceBuilder, ResponseBody, RouterRequest, Schema, ServiceBuilderExt,
};
use apollo_router_core::{prelude::*, Context};
use apollo_router_core::{DynPlugin, TowerSubgraphService};
use envmnt::types::ExpandOptions;
use envmnt::ExpansionType;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterService from configuration when it changes
#[async_trait::async_trait]
pub trait RouterServiceFactory: Send + Sync + 'static {
    type RouterService: Service<
            Request<graphql::Request>,
            Response = Response<ResponseBody>,
            Error = BoxError,
            Future = Self::Future,
        > + Send
        + Sync
        + Clone
        + 'static;
    type Future: Send;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<graphql::Schema>,
        previous_router: Option<&'a Self::RouterService>,
    ) -> Result<Self::RouterService, BoxError>;

    fn plugins(&self) -> &[(String, Box<dyn DynPlugin>)];
}

/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default)]
pub struct YamlRouterServiceFactory {
    plugins: Vec<(String, Box<dyn DynPlugin>)>,
}

impl Drop for YamlRouterServiceFactory {
    fn drop(&mut self) {
        // If we get here, everything is good so shutdown our old plugins
        // If we fail to shutdown a plugin, just log it and move on...
        for (_, mut plugin) in self.plugins.drain(..).rev() {
            if let Err(err) = futures::executor::block_on(plugin.shutdown()) {
                tracing::error!("could not stop plugin: {}, error: {}", plugin.name(), err);
            }
        }
    }
}

#[async_trait::async_trait]
impl RouterServiceFactory for YamlRouterServiceFactory {
    type RouterService = Buffer<
        BoxCloneService<Request<graphql::Request>, Response<ResponseBody>, BoxError>,
        Request<graphql::Request>,
    >;
    type Future = <Self::RouterService as Service<Request<graphql::Request>>>::Future;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        _previous_router: Option<&'a Self::RouterService>,
    ) -> Result<Self::RouterService, BoxError> {
        let mut builder = PluggableRouterServiceBuilder::new(schema.clone());
        if configuration.server.introspection {
            builder = builder.with_naive_introspection();
        }

        for (name, _) in schema.subgraphs() {
            let subgraph_service = BoxService::new(TowerSubgraphService::new(name.to_string()));

            builder = builder.with_subgraph_service(name, subgraph_service);
        }
        // Process the plugins.
        let plugins = process_plugins(configuration.clone()).await?;

        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        let (pluggable_router_service, plugins) = builder.build().await?;
        let mut previous_plugins = std::mem::replace(&mut self.plugins, plugins);
        let service = ServiceBuilder::new().buffered().service(
            pluggable_router_service
                .map_request(
                    |http_request: Request<apollo_router_core::Request>| RouterRequest {
                        context: Context::new().with_request(http_request),
                    },
                )
                .map_response(|response| response.response)
                .boxed_clone(),
        );

        // We're good to go with the new service. Let the plugins know that this is about to happen.
        // This is needed so that the Telemetry plugin can swap in the new propagator.
        // The alternative is that we introduce another service on Plugin that wraps the request
        // as a much earlier stage.
        for (_, plugin) in &mut self.plugins {
            tracing::debug!("activating plugin {}", plugin.name());
            #[allow(deprecated)]
            plugin.activate();
            tracing::debug!("activated plugin {}", plugin.name());
        }

        // If we get here, everything is good so shutdown our previous plugins
        for (_, mut plugin) in previous_plugins.drain(..).rev() {
            if let Err(err) = plugin.shutdown().await {
                // If we can't shutdown a plugin, we terminate the router since we can't
                // assume that it is safe to continue.
                tracing::error!("could not stop plugin: {}, error: {}", plugin.name(), err);
                tracing::error!("terminating router...");
                std::process::exit(1);
            }
        }
        Ok(service)
    }

    fn plugins(&self) -> &[(String, Box<dyn DynPlugin>)] {
        &self.plugins
    }
}

async fn process_plugins(
    configuration: Arc<Configuration>,
) -> Result<HashMap<String, Box<dyn DynPlugin>>, BoxError> {
    let mut errors = Vec::new();
    let plugin_registry = apollo_router_core::plugins();
    let mut plugin_instances = Vec::with_capacity(configuration.plugins().len());

    for (name, configuration) in configuration.plugins().iter() {
        let name = name.clone();
        match plugin_registry.get(name.as_str()) {
            Some(factory) => {
                tracing::debug!(
                    "creating plugin: '{}' with configuration:\n{:#}",
                    name,
                    configuration
                );

                // expand any env variables in the config before processing.
                let configuration = expand_env_variables(configuration);
                match factory.create_instance(&configuration) {
                    Ok(mut plugin) => {
                        tracing::debug!("starting plugin: {}", name);
                        match plugin.startup().await {
                            Ok(_v) => {
                                tracing::debug!("started plugin: {}", name);
                                plugin_instances.push((name.clone(), plugin));
                            }
                            Err(err) => errors.push(ConfigurationError::PluginStartup {
                                plugin: name,
                                error: err.to_string(),
                            }),
                        }
                    }
                    Err(err) => errors.push(ConfigurationError::PluginConfiguration {
                        plugin: name,
                        error: err.to_string(),
                    }),
                }
            }
            None => errors.push(ConfigurationError::PluginUnknown(name)),
        }
    }

    if !errors.is_empty() {
        // Shutdown all the plugins we started
        for (_plugin_name, plugin) in plugin_instances.iter_mut().rev() {
            tracing::debug!("stopping plugin: {}", plugin.name());
            if let Err(err) = plugin.shutdown().await {
                // If we can't shutdown a plugin, we terminate the router since we can't
                // assume that it is safe to continue.
                tracing::error!("could not stop plugin: {}, error: {}", plugin.name(), err);
                tracing::error!("terminating router...");
                std::process::exit(1);
            } else {
                tracing::debug!("stopped plugin: {}", plugin.name());
            }
        }

        for error in &errors {
            tracing::error!("{:#}", error);
        }

        Err(BoxError::from(
            errors
                .into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<String>>()
                .join("\n"),
        ))
    } else {
        Ok(plugin_instances.into_iter().collect())
    }
}

fn expand_env_variables(configuration: &serde_json::Value) -> serde_json::Value {
    let mut configuration = configuration.clone();
    visit(&mut configuration);
    configuration
}

fn visit(value: &mut serde_json::Value) {
    match value {
        Value::String(value) => {
            *value = envmnt::expand(
                value,
                Some(
                    ExpandOptions::new()
                        .clone_with_expansion_type(ExpansionType::UnixBracketsWithDefaults),
                ),
            );
        }
        Value::Array(a) => a.iter_mut().for_each(visit),
        Value::Object(o) => o.iter_mut().for_each(|(_, v)| visit(v)),
        _ => {}
    }
}

#[cfg(test)]
mod test {
    use crate::router_factory::RouterServiceFactory;
    use crate::{Configuration, YamlRouterServiceFactory};
    use apollo_router_core::Schema;
    use apollo_router_core::{register_plugin, Plugin};
    use schemars::JsonSchema;
    use serde::Deserialize;
    use std::error::Error;
    use std::fmt;
    use std::sync::Arc;
    use tower_http::BoxError;

    #[derive(Debug)]
    struct PluginError;

    impl fmt::Display for PluginError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "PluginError")
        }
    }

    impl Error for PluginError {}

    // Always starts and stops plugin

    #[derive(Debug)]
    struct AlwaysStartsAndStopsPlugin {}

    #[derive(Debug, Default, Deserialize, JsonSchema)]
    struct Conf {
        name: String,
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysStartsAndStopsPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            Ok(())
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::debug!("{}", configuration.name);
            Ok(AlwaysStartsAndStopsPlugin {})
        }
    }

    register_plugin!(
        "apollo.test",
        "always_starts_and_stops",
        AlwaysStartsAndStopsPlugin
    );

    // Always fails to start plugin

    #[derive(Debug)]
    struct AlwaysFailsToStartPlugin {}

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStartPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            Err(Box::new(PluginError {}))
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            Ok(())
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::debug!("{}", configuration.name);
            Ok(AlwaysFailsToStartPlugin {})
        }
    }

    register_plugin!(
        "apollo.test",
        "always_fails_to_start",
        AlwaysFailsToStartPlugin
    );

    // Always fails to stop plugin

    #[derive(Debug)]
    struct AlwaysFailsToStopPlugin {}

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStopPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            Err(Box::new(PluginError {}))
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::debug!("{}", configuration.name);
            Ok(AlwaysFailsToStopPlugin {})
        }
    }

    register_plugin!(
        "apollo.test",
        "always_fails_to_stop",
        AlwaysFailsToStopPlugin
    );

    // Always fails to stop plugin

    #[derive(Debug)]
    struct AlwaysFailsToStartAndStopPlugin {}

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStartAndStopPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            Err(Box::new(PluginError {}))
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            Err(Box::new(PluginError {}))
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::debug!("{}", configuration.name);
            Ok(AlwaysFailsToStartAndStopPlugin {})
        }
    }

    register_plugin!(
        "apollo.test",
        "always_fails_to_start_and_stop",
        AlwaysFailsToStartAndStopPlugin
    );

    #[tokio::test]
    async fn test_yaml_no_extras() {
        let config = Configuration::builder().build();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_starts_and_stops() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                apollo.test.always_starts_and_stops:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_fails_to_start() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                apollo.test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_fails_to_stop() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                apollo.test.always_fails_to_stop:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_fails_to_start_and_stop() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                apollo.test.always_fails_to_start_and_stop:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema: Schema = include_str!("testdata/supergraph.graphql").parse().unwrap();

        let service = YamlRouterServiceFactory::default()
            .create(Arc::new(config), Arc::new(schema), None)
            .await;
        service.map(|_| ())
    }
}
