// This entire file is license key functionality
use crate::configuration::{Configuration, ConfigurationError};
use crate::layers::ServiceBuilderExt;
use crate::plugin::DynPlugin;
use crate::services::Plugins;
use crate::SubgraphService;
use crate::{
    http_compat::{Request, Response},
    PluggableRouterServiceBuilder, ResponseBody, Schema,
};
use envmnt::types::ExpandOptions;
use envmnt::ExpansionType;
use futures::stream::BoxStream;
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
pub(crate) trait RouterServiceFactory: Send + Sync + 'static {
    type RouterService: Service<
            Request<crate::Request>,
            Response = Response<BoxStream<'static, ResponseBody>>,
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
        schema: Arc<crate::Schema>,
        previous_router: Option<&'a Self::RouterService>,
    ) -> Result<(Self::RouterService, Plugins), BoxError>;
}

/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default)]
pub(crate) struct YamlRouterServiceFactory;

#[async_trait::async_trait]
impl RouterServiceFactory for YamlRouterServiceFactory {
    type RouterService = Buffer<
        BoxCloneService<
            Request<crate::Request>,
            Response<BoxStream<'static, ResponseBody>>,
            BoxError,
        >,
        Request<crate::Request>,
    >;
    type Future = <Self::RouterService as Service<Request<crate::Request>>>::Future;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        _previous_router: Option<&'a Self::RouterService>,
    ) -> Result<(Self::RouterService, Plugins), BoxError> {
        let mut builder = PluggableRouterServiceBuilder::new(schema.clone());
        if configuration.server.introspection {
            builder = builder.with_naive_introspection();
        }

        for (name, _) in schema.subgraphs() {
            let subgraph_service = BoxService::new(SubgraphService::new(name.to_string()));

            builder = builder.with_subgraph_service(name, subgraph_service);
        }
        // Process the plugins.
        let plugins = create_plugins(&configuration, &schema).await?;

        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        let (pluggable_router_service, mut plugins) = builder.build().await?;
        let service = ServiceBuilder::new().buffered().service(
            pluggable_router_service
                .map_request(|http_request: Request<crate::Request>| http_request.into())
                .map_response(|response| response.response)
                .boxed_clone(),
        );

        // We're good to go with the new service. Let the plugins know that this is about to happen.
        // This is needed so that the Telemetry plugin can swap in the new propagator.
        // The alternative is that we introduce another service on Plugin that wraps the request
        // at a much earlier stage.
        for (_, plugin) in &mut plugins {
            tracing::debug!("activating plugin {}", plugin.name());
            plugin.activate();
            tracing::debug!("activated plugin {}", plugin.name());
        }

        Ok((service, plugins))
    }
}

async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
) -> Result<HashMap<String, Box<dyn DynPlugin>>, BoxError> {
    let mut errors = Vec::new();
    let plugin_registry = crate::plugin::plugins();
    let mut plugin_instances = Vec::new();

    for (name, mut configuration) in configuration.plugins().into_iter() {
        // Ugly hack to get the schema sha into the the telemetry plugin

        let name = name.clone();
        match plugin_registry.get(name.as_str()) {
            Some(factory) => {
                tracing::debug!(
                    "creating plugin: '{}' with configuration:\n{:#}",
                    name,
                    configuration
                );
                if name == "apollo.telemetry" {
                    inject_schema_id(schema, &mut configuration)
                }
                // expand any env variables in the config before processing.
                let configuration = expand_env_variables(&configuration);
                match factory.create_instance(&configuration).await {
                    Ok(plugin) => {
                        plugin_instances.push((name.clone(), plugin));
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

fn inject_schema_id(schema: &Schema, configuration: &mut Value) {
    if configuration.get("apollo").is_none() {
        if let Some(telemetry) = configuration.as_object_mut() {
            telemetry.insert("apollo".to_string(), Value::Object(Default::default()));
        }
    }

    if let (Some(schema_id), Some(apollo)) = (
        &schema.api_schema().schema_id,
        configuration.get_mut("apollo"),
    ) {
        if let Some(apollo) = apollo.as_object_mut() {
            apollo.insert(
                "schema_id".to_string(),
                Value::String(schema_id.to_string()),
            );
        }
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
    use crate::configuration::Configuration;
    use crate::plugin::Plugin;
    use crate::register_plugin;
    use crate::router_factory::YamlRouterServiceFactory;
    use crate::router_factory::{inject_schema_id, RouterServiceFactory};
    use crate::Schema;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;
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

        async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
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

        async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
            tracing::debug!("{}", configuration.name);
            Err(BoxError::from("Error"))
        }
    }

    register_plugin!(
        "apollo.test",
        "always_fails_to_start",
        AlwaysFailsToStartPlugin
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
    async fn test_yaml_plugins_combo_start_and_fail() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                apollo.test.always_starts_and_stops:
                    name: albert
                apollo.test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    // This test must use the multi_thread tokio executor or the opentelemetry hang bug will
    // be encountered. (See https://github.com/open-telemetry/opentelemetry-rust/issues/536)
    #[tokio::test(flavor = "multi_thread")]
    async fn test_telemetry_doesnt_hang_with_invalid_schema() {
        use crate::subscriber::{set_global_subscriber, RouterSubscriber};
        use tracing_subscriber::EnvFilter;

        // A global subscriber must be set before we start up the telemetry plugin
        let _ = set_global_subscriber(RouterSubscriber::JsonSubscriber(
            tracing_subscriber::fmt::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .json()
                .finish(),
        ));

        let config: Configuration = serde_yaml::from_str(
            r#"
            telemetry:
              tracing:
                trace_config:
                  service_name: router
                otlp:
                  endpoint: default
        "#,
        )
        .unwrap();

        let schema: Schema = include_str!("testdata/invalid_supergraph.graphql")
            .parse()
            .unwrap();

        let service = YamlRouterServiceFactory::default()
            .create(Arc::new(config), Arc::new(schema), None)
            .await;
        service.map(|_| ()).unwrap_err();
    }

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema: Schema = include_str!("testdata/supergraph.graphql").parse().unwrap();

        let service = YamlRouterServiceFactory::default()
            .create(Arc::new(config), Arc::new(schema), None)
            .await;
        service.map(|_| ())
    }

    #[test]
    fn test_inject_schema_id() {
        let schema = include_str!("testdata/starstuff@current.graphql")
            .parse()
            .unwrap();
        let mut config = json!({});
        inject_schema_id(&schema, &mut config);
        let config =
            serde_json::from_value::<crate::plugins::telemetry::config::Conf>(config).unwrap();
        assert_eq!(
            &config.apollo.unwrap().schema_id,
            "ba573b479c8b3fa273f439b26b9eda700152341d897f18090d52cd073b15f909"
        );
    }
}
