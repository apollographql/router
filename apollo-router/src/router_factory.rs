use crate::configuration::{Configuration, ConfigurationError};
use apollo_router_core::deduplication::QueryDeduplicationLayer;
use apollo_router_core::{
    http_compat::{Request, Response},
    PluggableRouterServiceBuilder, ResponseBody, RouterRequest, Schema,
};
use apollo_router_core::{prelude::*, Context};
use apollo_router_core::{DynPlugin, ReqwestSubgraphService};
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::{BoxCloneService, BoxService};
use tower::{BoxError, Layer, ServiceBuilder, ServiceExt};
use tower_service::Service;

const REPORTING_MODULE_NAME: &str = "com.apollographql.reporting";

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
        let mut errors: Vec<ConfigurationError> = Vec::default();
        let configuration = (*configuration).clone();

        let configuration = add_default_plugins(configuration);

        let mut builder = PluggableRouterServiceBuilder::new(schema.clone());

        for (name, _) in schema.subgraphs() {
            let dedup_layer = QueryDeduplicationLayer;
            let subgraph_service =
                BoxService::new(dedup_layer.layer(ReqwestSubgraphService::new(name.to_string())));

            builder = builder.with_subgraph_service(name, subgraph_service);
        }
        {
            async fn process_plugin(
                mut builder: PluggableRouterServiceBuilder,
                errors: &mut Vec<ConfigurationError>,
                name: String,
                configuration: &serde_json::Value,
            ) -> PluggableRouterServiceBuilder {
                let plugin_registry = apollo_router_core::plugins();
                match plugin_registry.get(name.as_str()) {
                    Some(factory) => match factory.create_instance(configuration) {
                        Ok(mut plugin) => match plugin.startup().await {
                            Ok(_v) => {
                                builder = builder.with_dyn_plugin(plugin);
                            }
                            Err(err) => {
                                tracing::error!("starting plugin: {}, error: {}", name, err);
                                (*errors).push(ConfigurationError::PluginStartup {
                                    plugin: name,
                                    error: err.to_string(),
                                });
                            }
                        },
                        Err(err) => {
                            (*errors).push(ConfigurationError::PluginConfiguration {
                                plugin: name,
                                error: err.to_string(),
                            });
                        }
                    },
                    None => {
                        (*errors).push(ConfigurationError::PluginUnknown(name));
                    }
                }
                builder
            }

            // If it was required, we ensured that the Reporting plugin was in the
            // list of plugins above. Now make sure that we process that plugin
            // before any other plugins.
            let already_processed = match configuration.plugins.plugins.get(REPORTING_MODULE_NAME) {
                Some(reporting_configuration) => {
                    builder = process_plugin(
                        builder,
                        &mut errors,
                        REPORTING_MODULE_NAME.to_string(),
                        reporting_configuration,
                    )
                    .await;
                    vec![REPORTING_MODULE_NAME]
                }
                None => vec![],
            };

            // Process the remaining plugins. We use already_processed to skip
            // those plugins we already processed.
            for (name, configuration) in configuration
                .plugins
                .plugins
                .iter()
                .filter(|(name, _)| !already_processed.contains(&name.as_str()))
            {
                let name = name.clone();
                builder = process_plugin(builder, &mut errors, name, configuration).await;
            }
        }
        if !errors.is_empty() {
            // Shutdown all the plugins we started
            for plugin in builder.plugins().iter_mut().rev() {
                if let Err(err) = plugin.shutdown().await {
                    // If we can't shutdown a plugin, we terminate the router since we can't
                    // assume that it is safe to continue.
                    tracing::error!("could not stop plugin: {}, error: {}", plugin.name(), err);
                    tracing::error!("terminating router...");
                    std::process::exit(1);
                }
            }
            for error in errors {
                tracing::error!("{:#}", error);
            }
            return Err(Box::new(ConfigurationError::InvalidConfiguration));
        }

        // This **must** run after:
        //  - the Reporting plugin is initialized.
        //  - all configuration errors are checked
        // and **before** build() is called.
        //
        // This is because our tracing configuration is initialized by
        // the startup() method of our Reporting plugin.
        let (pluggable_router_service, plugins) = builder.build().await;
        let mut previous_plugins = std::mem::replace(&mut self.plugins, plugins);
        let service = ServiceBuilder::new().buffer(20_000).service(
            pluggable_router_service
                .map_request(
                    |http_request: Request<apollo_router_core::Request>| RouterRequest {
                        context: Context::new().with_request(http_request),
                    },
                )
                .map_response(|response| response.response)
                .boxed_clone(),
        );
        // If we get here, everything is good so shutdown our previous plugins
        for mut plugin in previous_plugins.drain(..).rev() {
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
}

fn add_default_plugins(mut configuration: Configuration) -> Configuration {
    // Because studio usage reporting requires the Reporting plugin,
    // we must force the addition of the Reporting plugin if APOLLO_KEY
    // is set.
    if std::env::var("APOLLO_KEY").is_ok() {
        // If the user has not specified Reporting configuration, then
        // insert a valid "minimal" configuration which allows
        // studio usage reporting to function
        if !configuration
            .plugins
            .plugins
            .contains_key(REPORTING_MODULE_NAME)
        {
            configuration.plugins.plugins.insert(
                REPORTING_MODULE_NAME.to_string(),
                serde_json::json!({ "opentelemetry": null }),
            );
        }
    }

    configuration
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
        "com.apollographql.test",
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
        "com.apollographql.test",
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
        "com.apollographql.test",
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
        "com.apollographql.test",
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
                com.apollographql.test.always_starts_and_stops:
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
                com.apollographql.test.always_fails_to_start:
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
                com.apollographql.test.always_fails_to_stop:
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
                com.apollographql.test.always_fails_to_start_and_stop:
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
