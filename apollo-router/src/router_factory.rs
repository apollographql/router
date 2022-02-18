use crate::configuration::{Configuration, ConfigurationError};
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::deduplication::QueryDeduplicationLayer;
use apollo_router_core::DynPlugin;
use apollo_router_core::{
    http_compat::{Request, Response},
    PluggableRouterServiceBuilder, ResponseBody, RouterRequest, Schema,
};
use apollo_router_core::{prelude::*, Context};
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};
use tower_service::Service;
use tracing::instrument::WithSubscriber;

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
}

/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default)]
pub struct YamlRouterServiceFactory {
    plugins: Vec<Box<dyn DynPlugin>>,
}

impl Drop for YamlRouterServiceFactory {
    fn drop(&mut self) {
        // If we get here, everything is good so shutdown our old plugins
        // If we fail to shutdown a plugin, just log it and move on...
        for mut plugin in self.plugins.drain(..).rev() {
            if let Err(err) = futures::executor::block_on(plugin.shutdown()) {
                tracing::error!("could not stop plugin: {}, error: {}", plugin, err);
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
        let mut errors: Vec<ConfigurationError> = Vec::default();
        let mut configuration = (*configuration).clone();
        if let Err(mut e) = configuration.load_subgraphs(&schema) {
            errors.append(&mut e);
        }

        let dispatcher = configuration
            .subscriber
            .clone()
            .map(tracing::Dispatch::new)
            .unwrap_or_default();
        let buffer = 20000;
        let mut builder = PluggableRouterServiceBuilder::new(schema, buffer, dispatcher.clone());

        for (name, subgraph) in &configuration.subgraphs {
            let dedup_layer = QueryDeduplicationLayer;
            let mut subgraph_service = BoxService::new(dedup_layer.layer(
                ReqwestSubgraphService::new(name.to_string(), subgraph.routing_url.clone()),
            ));

            for layer in &subgraph.layers {
                match layer.as_object().and_then(|o| o.iter().next()) {
                    Some((kind, config)) => match apollo_router_core::layers().get(kind) {
                        None => {
                            errors.push(ConfigurationError::LayerUnknown(kind.to_owned()));
                        }
                        Some(factory) => match factory.create_instance(config) {
                            Ok(layer) => subgraph_service = layer.layer(subgraph_service),
                            Err(err) => errors.push(ConfigurationError::LayerConfiguration {
                                layer: kind.to_string(),
                                error: err.to_string(),
                            }),
                        },
                    },
                    None => errors.push(ConfigurationError::LayerConfiguration {
                        layer: "unknown".into(),
                        error: "layer must be an object".into(),
                    }),
                }
            }

            builder = builder.with_subgraph_service(name, subgraph_service);
        }
        {
            let plugin_registry = apollo_router_core::plugins();
            for (name, configuration) in &configuration.plugins.plugins {
                let name = name.as_str().to_string();
                match plugin_registry.get(name.as_str()) {
                    Some(factory) => match factory.create_instance(configuration) {
                        Ok(mut plugin) => match plugin.startup().await {
                            Ok(_v) => {
                                builder = builder.with_dyn_plugin(plugin);
                            }
                            Err(err) => {
                                tracing::error!("starting plugin: {}, failed: {}", name, err);
                                errors.push(ConfigurationError::PluginStartup {
                                    plugin: name,
                                    error: err.to_string(),
                                });
                            }
                        },
                        Err(err) => {
                            errors.push(ConfigurationError::PluginConfiguration {
                                plugin: name,
                                error: err.to_string(),
                            });
                        }
                    },
                    None => {
                        errors.push(ConfigurationError::PluginUnknown(name));
                    }
                }
            }
        }
        if !errors.is_empty() {
            // Shutdown all the plugins we started
            for plugin in builder.plugins().iter_mut().rev() {
                if let Err(err) = plugin.shutdown().await {
                    tracing::error!("could not stop plugin: {}, error: {}", plugin, err);
                    tracing::error!("terminating router...");
                    std::process::exit(1);
                }
            }
            for error in errors {
                tracing::error!("{:#}", error);
            }
            return Err(Box::new(ConfigurationError::InvalidConfiguration));
        }

        let (pluggable_router_service, plugins) = builder.build().await;
        let mut previous_plugins = std::mem::replace(&mut self.plugins, plugins);
        let (service, worker) = Buffer::pair(
            ServiceBuilder::new().service(
                pluggable_router_service
                    .map_request(|http_request: Request<apollo_router_core::Request>| {
                        RouterRequest {
                            context: Context::new().with_request(http_request),
                        }
                    })
                    .map_response(|response| response.response)
                    .boxed_clone(),
            ),
            buffer,
        );
        tokio::spawn(worker.with_subscriber(dispatcher));
        // If we get here, everything is good so shutdown our previous plugins
        for mut plugin in previous_plugins.drain(..).rev() {
            if let Err(err) = plugin.shutdown().await {
                tracing::error!("could not stop plugin: {}, error: {}", plugin, err);
                tracing::error!("terminating router...");
                std::process::exit(1);
            }
        }
        Ok(service)
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

    impl fmt::Display for AlwaysStartsAndStopsPlugin {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AlwaysStartsAndStopsPlugin")
        }
    }

    #[derive(Debug, Default, Deserialize, JsonSchema)]
    struct Conf {
        name: String,
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysStartsAndStopsPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            tracing::info!("starting: {}", stringify!(AlwaysStartsAndStopsPlugin));
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            tracing::info!("shutting down: {}", stringify!(AlwaysStartsAndStopsPlugin));
            Ok(())
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::info!("Hello {}!", configuration.name);
            Ok(AlwaysStartsAndStopsPlugin {})
        }
    }

    register_plugin!(
        "com.apollo.test",
        "always_starts_and_stops",
        AlwaysStartsAndStopsPlugin
    );

    // Always fails to start plugin

    #[derive(Debug)]
    struct AlwaysFailsToStartPlugin {}

    impl fmt::Display for AlwaysFailsToStartPlugin {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AlwaysFailsToStartPlugin")
        }
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStartPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            tracing::info!("starting: {}", stringify!(AlwaysFailsToStartPlugin));
            Err(Box::new(PluginError {}))
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            tracing::info!("shutting down: {}", stringify!(AlwaysFailsToStartPlugin));
            Ok(())
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::info!("Hello {}!", configuration.name);
            Ok(AlwaysFailsToStartPlugin {})
        }
    }

    register_plugin!(
        "com.apollo.test",
        "always_fails_to_start",
        AlwaysFailsToStartPlugin
    );

    // Always fails to stop plugin

    #[derive(Debug)]
    struct AlwaysFailsToStopPlugin {}

    impl fmt::Display for AlwaysFailsToStopPlugin {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AlwaysFailsToStopPlugin")
        }
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStopPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            tracing::info!("starting: {}", stringify!(AlwaysFailsToStopPlugin));
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            tracing::info!("shutting down: {}", stringify!(AlwaysFailsToStopPlugin));
            Err(Box::new(PluginError {}))
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::info!("Hello {}!", configuration.name);
            Ok(AlwaysFailsToStopPlugin {})
        }
    }

    register_plugin!(
        "com.apollo.test",
        "always_fails_to_stop",
        AlwaysFailsToStopPlugin
    );

    // Always fails to stop plugin

    #[derive(Debug)]
    struct AlwaysFailsToStartAndStopPlugin {}

    impl fmt::Display for AlwaysFailsToStartAndStopPlugin {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AlwaysFailsToStartAndStopPlugin")
        }
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStartAndStopPlugin {
        type Config = Conf;

        async fn startup(&mut self) -> Result<(), BoxError> {
            tracing::info!("starting: {}", stringify!(AlwaysFailsToStartAndStopPlugin));
            Err(Box::new(PluginError {}))
        }

        async fn shutdown(&mut self) -> Result<(), BoxError> {
            tracing::info!(
                "shutting down: {}",
                stringify!(AlwaysFailsToStartAndStopPlugin)
            );
            Err(Box::new(PluginError {}))
        }

        fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::info!("Hello {}!", configuration.name);
            Ok(AlwaysFailsToStartAndStopPlugin {})
        }
    }

    register_plugin!(
        "com.apollo.test",
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
    #[test_log::test]
    async fn test_yaml_layers() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            subgraphs:
                foo:
                    routing_url: https://foo
                    layers:
                        - headers_insert:
                              name: "foo"
                              value: "foo"
                            
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_starts_and_stops() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                com.apollo.test.always_starts_and_stops:
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
                com.apollo.test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(!service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_fails_to_stop() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                com.apollo.test.always_fails_to_stop:
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
                com.apollo.test.always_fails_to_start_and_stop:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(!service.is_ok())
    }

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema: Schema = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1"),
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        {
        query: Query
        mutation: Mutation
        }"#
        .parse()
        .unwrap();

        let service = YamlRouterServiceFactory::default()
            .create(Arc::new(config), Arc::new(schema), None)
            .await;
        service.map(|_| ())
    }
}
