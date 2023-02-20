use std::io;
// With regards to ELv2 licensing, this entire file is license key functionality
use std::sync::Arc;

use axum::response::IntoResponse;
use http::StatusCode;
use multimap::MultiMap;
use once_cell::sync::Lazy;
use rustls::RootCertStore;
use serde_json::Map;
use serde_json::Value;
use tower::service_fn;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::configuration::TlsSubgraph;
use crate::plugin::DynPlugin;
use crate::plugin::Handler;
use crate::plugin::PluginFactory;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router_service::RouterCreator;
use crate::services::transport;
use crate::services::PluggableSupergraphServiceBuilder;
use crate::services::SubgraphService;
use crate::services::SupergraphCreator;
use crate::spec::Schema;
use crate::ListenAddr;

#[derive(Clone)]
/// A path and a handler to be exposed as a web_endpoint for plugins
pub struct Endpoint {
    pub(crate) path: String,
    // Plugins need to be Send + Sync
    // BoxCloneService isn't enough
    handler: Handler,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("path", &self.path)
            .finish()
    }
}

impl Endpoint {
    /// Creates an Endpoint given a path and a Boxed Service
    #[deprecated = "use `from_router_service` instead"]
    #[allow(deprecated)]
    pub fn new(path: String, handler: transport::BoxService) -> Self {
        let router_service = ServiceBuilder::new()
            .map_request(|request: router::Request| request.router_request)
            .map_response(|response: transport::Response| response.into())
            .service(handler)
            .boxed();
        Self {
            path,
            handler: Handler::new(router_service),
        }
    }

    /// Creates an Endpoint given a path and a Boxed Service
    pub fn from_router_service(path: String, handler: router::BoxService) -> Self {
        Self {
            path,
            handler: Handler::new(handler),
        }
    }
    pub(crate) fn into_router(self) -> axum::Router {
        let handler = move |req: http::Request<hyper::Body>| {
            let endpoint = self.handler.clone();
            async move {
                Ok(endpoint
                    .oneshot(req.into())
                    .await
                    .map(|res| res.response)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
                    .into_response())
            }
        };
        axum::Router::new().route_service(self.path.as_str(), service_fn(handler))
    }
}
/// Factory for creating a RouterService
///
/// Instances of this traits are used by the HTTP server to generate a new
/// RouterService on each request
pub(crate) trait RouterFactory:
    ServiceFactory<router::Request, Service = Self::RouterService> + Clone + Send + Sync + 'static
{
    type RouterService: Service<
            router::Request,
            Response = router::Response,
            Error = BoxError,
            Future = Self::Future,
        > + Send;
    type Future: Send;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
}

/// Factory for creating a RouterFactory
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterFactory from configuration when it changes
#[async_trait::async_trait]
pub(crate) trait RouterSuperServiceFactory: Send + Sync + 'static {
    type RouterFactory: RouterFactory;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError>;
}

/// Main implementation of the SupergraphService factory, supporting the extensions system
#[derive(Default)]
pub(crate) struct YamlRouterFactory;

#[async_trait::async_trait]
impl RouterSuperServiceFactory for YamlRouterFactory {
    type RouterFactory = RouterCreator<SupergraphCreator>;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError> {
        // Process the plugins.
        let plugins = create_plugins(&configuration, &schema, extra_plugins).await?;

        let tls_root_store: Option<RootCertStore> = configuration
            .tls
            .subgraph
            .all
            .create_certificate_store()
            .transpose()?;

        let mut builder = PluggableSupergraphServiceBuilder::new(schema.clone());
        builder = builder.with_configuration(configuration.clone());

        for (name, _) in schema.subgraphs() {
            let subgraph_root_store = configuration
                .tls
                .subgraph
                .subgraphs
                .get(name)
                .as_ref()
                .and_then(|subgraph| subgraph.create_certificate_store())
                .transpose()?
                .or_else(|| tls_root_store.clone());

            let subgraph_service = match plugins
                .iter()
                .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
                .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<TrafficShaping>())
            {
                Some(shaping) => Either::A(
                    shaping.subgraph_service_internal(
                        name,
                        SubgraphService::new(
                            name,
                            configuration
                                .supergraph
                                .apq
                                .subgraph
                                .subgraphs
                                .get(name)
                                .map(|apq| apq.enabled)
                                .unwrap_or(configuration.supergraph.apq.subgraph.all.enabled),
                            subgraph_root_store,
                        ),
                    ),
                ),
                None => Either::B(SubgraphService::new(name, false, subgraph_root_store)),
            };
            builder = builder.with_subgraph_service(name, subgraph_service);
        }

        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        // Final creation after this line we must NOT fail to go live with the new router from this point as some plugins may interact with globals.
        let mut supergraph_creator = builder.build().await?;

        if let Some(router) = previous_router {
            if configuration.supergraph.query_planning.warmed_up_queries > 0 {
                let cache_keys = router
                    .cache_keys(configuration.supergraph.query_planning.warmed_up_queries)
                    .await;

                if !cache_keys.is_empty() {
                    tracing::info!(
                        "warming up the query plan cache with {} queries, this might take a while",
                        cache_keys.len()
                    );

                    supergraph_creator.warm_up_query_planner(cache_keys).await;
                }
            }
        }

        Ok(Self::RouterFactory::new(Arc::new(supergraph_creator), &configuration).await)
    }
}

impl YamlRouterFactory {
    pub(crate) async fn create_supergraph<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        _previous_router: Option<&'a SupergraphCreator>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<SupergraphCreator, BoxError> {
        // Process the plugins.
        let plugins = create_plugins(&configuration, &schema, extra_plugins).await?;

        let tls_root_store = configuration
            .tls
            .subgraph
            .all
            .create_certificate_store()
            .transpose()?;

        let mut builder = PluggableSupergraphServiceBuilder::new(schema.clone());
        builder = builder.with_configuration(configuration.clone());

        for (name, _) in schema.subgraphs() {
            let subgraph_root_store = configuration
                .tls
                .subgraph
                .subgraphs
                .get(name)
                .as_ref()
                .and_then(|subgraph| subgraph.create_certificate_store())
                .transpose()?
                .or_else(|| tls_root_store.clone());

            let subgraph_service = match plugins
                .iter()
                .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
                .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<TrafficShaping>())
            {
                Some(shaping) => Either::A(
                    shaping.subgraph_service_internal(
                        name,
                        SubgraphService::new(
                            name,
                            configuration
                                .supergraph
                                .apq
                                .subgraph
                                .subgraphs
                                .get(name)
                                .map(|apq| apq.enabled)
                                .unwrap_or(configuration.supergraph.apq.subgraph.all.enabled),
                            subgraph_root_store,
                        ),
                    ),
                ),
                None => Either::B(SubgraphService::new(name, false, subgraph_root_store)),
            };
            builder = builder.with_subgraph_service(name, subgraph_service);
        }

        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        builder.build().await.map_err(BoxError::from)
    }
}

impl TlsSubgraph {
    fn create_certificate_store(&self) -> Option<Result<RootCertStore, ConfigurationError>> {
        self.certificate_authorities
            .as_deref()
            .map(create_certificate_store)
    }
}

fn create_certificate_store(
    certificate_authorities: &str,
) -> Result<RootCertStore, ConfigurationError> {
    let mut store = RootCertStore::empty();
    let certificates = load_certs(certificate_authorities).map_err(|e| {
        ConfigurationError::CertificateAuthorities {
            error: format!("could not parse the certificate list: {e}"),
        }
    })?;
    for certificate in certificates {
        store
            .add(&certificate)
            .map_err(|e| ConfigurationError::CertificateAuthorities {
                error: format!("could not add certificate to root store: {e}"),
            })?;
    }
    if store.is_empty() {
        Err(ConfigurationError::CertificateAuthorities {
            error: "the certificate list is empty".to_string(),
        })
    } else {
        Ok(store)
    }
}

fn load_certs(certificates: &str) -> io::Result<Vec<rustls::Certificate>> {
    tracing::debug!("loading root certificates");

    // Load and return certificate.
    let certs = rustls_pemfile::certs(&mut certificates.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::Other,
            "failed to load certificate".to_string(),
        )
    })?;
    Ok(certs.into_iter().map(rustls::Certificate).collect())
}

/// test only helper method to create a router factory in integration tests
///
/// not meant to be used directly
pub async fn create_test_service_factory_from_yaml(schema: &str, configuration: &str) {
    let config: Configuration = serde_yaml::from_str(configuration).unwrap();

    let schema: Schema = Schema::parse(schema, &Default::default()).unwrap();

    let service = YamlRouterFactory::default()
        .create(Arc::new(config), Arc::new(schema), None, None)
        .await;
    assert_eq!(
        service.map(|_| ()).unwrap_err().to_string().as_str(),
        r#"couldn't build Router Service: couldn't instantiate query planner; invalid schema: schema validation errors: Error extracting subgraphs from the supergraph: this might be due to errors in subgraphs that were mistakenly ignored by federation 0.x versions but are rejected by federation 2.
Please try composing your subgraphs with federation 2: this should help precisely pinpoint the problems and, once fixed, generate a correct federation 2 supergraph.

Details:
Error: Cannot find type "Review" in subgraph "products"
caused by
"#
    );
}

pub(crate) async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
) -> Result<Vec<(String, Box<dyn DynPlugin>)>, BoxError> {
    // List of mandatory plugins. Ordering is important!!
    let mandatory_plugins = vec![
        "apollo.include_subgraph_errors",
        "apollo.csrf",
        "apollo.telemetry",
    ];

    let mut errors = Vec::new();
    let plugin_registry: Vec<&'static Lazy<PluginFactory>> = crate::plugin::plugins().collect();
    let mut plugin_instances = Vec::new();
    let extra = extra_plugins.unwrap_or_default();

    for (name, mut configuration) in configuration.plugins().into_iter() {
        if extra.iter().any(|(n, _)| *n == name) {
            // An instance of this plugin was already added through TestHarness::extra_plugin
            continue;
        }

        match plugin_registry.iter().find(|factory| factory.name == name) {
            Some(factory) => {
                tracing::debug!(
                    "creating plugin: '{}' with configuration:\n{:#}",
                    name,
                    configuration
                );
                if name == "apollo.telemetry" {
                    inject_schema_id(schema, &mut configuration);
                }
                match factory
                    .create_instance(&configuration, schema.as_string().clone())
                    .await
                {
                    Ok(plugin) => {
                        plugin_instances.push((name, plugin));
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
    plugin_instances.extend(extra);

    // At this point we've processed all of the plugins that were provided in configuration.
    // We now need to do process our list of mandatory plugins:
    //  - If a mandatory plugin is already in the list, then it must be re-located
    //    to its mandatory location
    //  - If it is missing, it must be added at its mandatory location

    for (desired_position, name) in mandatory_plugins.iter().enumerate() {
        let position_maybe = plugin_instances.iter().position(|(x, _)| x == name);
        match position_maybe {
            Some(actual_position) => {
                // Found it, re-locate if required.
                if actual_position != desired_position {
                    let temp = plugin_instances.remove(actual_position);
                    plugin_instances.insert(desired_position, temp);
                }
            }
            None => {
                // Didn't find it, insert
                match plugin_registry
                    .iter()
                    .find(|factory| factory.name == **name)
                {
                    // Create an instance
                    Some(factory) => {
                        // Create default (empty) config
                        let mut config = Value::Object(Map::new());
                        // The apollo.telemetry" plugin isn't happy with empty config, so we
                        // give it some. If any of the other mandatory plugins need special
                        // treatment, then we'll have to perform it here.
                        // This is *required* by the telemetry module or it will fail...
                        if *name == "apollo.telemetry" {
                            inject_schema_id(schema, &mut config);
                        }
                        match factory
                            .create_instance(&config, schema.as_string().clone())
                            .await
                        {
                            Ok(plugin) => {
                                plugin_instances.insert(
                                    desired_position.min(plugin_instances.len()),
                                    (name.to_string(), plugin),
                                );
                            }
                            Err(err) => errors.push(ConfigurationError::PluginConfiguration {
                                plugin: name.to_string(),
                                error: err.to_string(),
                            }),
                        }
                    }
                    None => errors.push(ConfigurationError::PluginUnknown(name.to_string())),
                }
            }
        }
    }

    let plugin_details = plugin_instances
        .iter()
        .map(|(name, plugin)| (name, plugin.name()))
        .collect::<Vec<(&String, &str)>>();
    tracing::debug!(
        "plugins list: {:?}",
        plugin_details
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<&&String>>()
    );

    if !errors.is_empty() {
        for error in &errors {
            tracing::error!("{:#}", error);
        }

        Err(BoxError::from(format!(
            "there were {} configuration errors",
            errors.len()
        )))
    } else {
        Ok(plugin_instances)
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

#[cfg(test)]
mod test {
    use std::error::Error;
    use std::fmt;
    use std::sync::Arc;

    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;
    use tower_http::BoxError;

    use crate::configuration::Configuration;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::register_plugin;
    use crate::router_factory::inject_schema_id;
    use crate::router_factory::RouterSuperServiceFactory;
    use crate::router_factory::YamlRouterFactory;
    use crate::spec::Schema;

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

    /// Configuration for the test plugin
    #[derive(Debug, Default, Deserialize, JsonSchema)]
    struct Conf {
        /// The name of the test
        name: String,
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysStartsAndStopsPlugin {
        type Config = Conf;

        async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            tracing::debug!("{}", init.config.name);
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

        async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            tracing::debug!("{}", init.config.name);
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
        let config = Configuration::builder().build().unwrap();
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

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &config).unwrap();

        let service = YamlRouterFactory::default()
            .create(Arc::new(config), Arc::new(schema), None, None)
            .await;
        service.map(|_| ())
    }

    #[test]
    fn test_inject_schema_id() {
        let schema = include_str!("testdata/starstuff@current.graphql");
        let schema = Schema::parse(schema, &Default::default()).unwrap();
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
