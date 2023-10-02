use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use axum::response::IntoResponse;
use futures::future::BoxFuture;
use http::StatusCode;
use indexmap::IndexMap;
use multimap::MultiMap;
use rustls::RootCertStore;
use serde_json::Map;
use serde_json::Value;
use tower::retry::Retry;
use tower::service_fn;
use tower::util::Either;
use tower::util::Oneshot;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::configuration::TlsSubgraph;
use crate::configuration::APOLLO_PLUGIN_PREFIX;
use crate::plugin::DynPlugin;
use crate::plugin::Handler;
use crate::plugin::PluginFactory;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::plugins::traffic_shaping::rate;
use crate::plugins::traffic_shaping::timeout;
use crate::plugins::traffic_shaping::RetryPolicy;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router_service::RouterCreator;
use crate::services::subgraph;
use crate::services::transport;
use crate::services::HasConfig;
use crate::services::HasSchema;
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
        schema: String,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError>;
}

/// Main implementation of the SupergraphService factory, supporting the extensions system
#[derive(Default)]
pub(crate) struct YamlRouterFactory;

#[async_trait::async_trait]
impl RouterSuperServiceFactory for YamlRouterFactory {
    type RouterFactory = RouterCreator;

    async fn create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: String,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<Self::RouterFactory, BoxError> {
        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner = match previous_router.as_ref().map(|router| router.planner()) {
            None => BridgeQueryPlanner::new(schema.clone(), configuration.clone()).await?,
            Some(planner) => {
                BridgeQueryPlanner::new_from_planner(planner, schema.clone(), configuration.clone())
                    .await?
            }
        };

        let schema_changed = previous_router
            .map(|router| router.supergraph_creator.schema().raw_sdl.as_ref() == &schema)
            .unwrap_or_default();

        let config_changed = previous_router
            .map(|router| router.supergraph_creator.config() == configuration)
            .unwrap_or_default();

        if config_changed {
            configuration
                .notify
                .broadcast_configuration(Arc::downgrade(&configuration));
        }

        let schema = bridge_query_planner.schema();
        if schema_changed {
            configuration.notify.broadcast_schema(schema.clone());
        }

        // Process the plugins.
        let plugins = create_plugins(&configuration, &schema, extra_plugins).await?;

        let mut builder = PluggableSupergraphServiceBuilder::new(bridge_query_planner);
        builder = builder.with_configuration(configuration.clone());
        let subgraph_services = create_subgraph_services(&plugins, &schema, &configuration).await?;
        for (name, subgraph_service) in subgraph_services {
            builder = builder.with_subgraph_service(&name, subgraph_service);
        }
        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        // Final creation after this line we must NOT fail to go live with the new router from this point as some plugins may interact with globals.
        let mut supergraph_creator = builder.build().await?;

        // Instantiate the parser here so we can use it to warm up the planner below
        let query_analysis_layer =
            QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&configuration)).await;

        let persisted_query_layer = Arc::new(PersistedQueryLayer::new(&configuration).await?);

        if let Some(previous_router) = previous_router {
            let cache_keys = previous_router
                .cache_keys(configuration.supergraph.query_planning.warmed_up_queries)
                .await;

            supergraph_creator
                .warm_up_query_planner(&query_analysis_layer, &persisted_query_layer, cache_keys)
                .await;
        };

        Ok(Self::RouterFactory::new(
            query_analysis_layer,
            persisted_query_layer,
            Arc::new(supergraph_creator),
            configuration,
        )
        .await?)
    }
}

pub(crate) async fn create_subgraph_services(
    plugins: &[(String, Box<dyn DynPlugin>)],
    schema: &Schema,
    configuration: &Configuration,
) -> Result<
    IndexMap<
        String,
        impl Service<
                subgraph::Request,
                Response = subgraph::Response,
                Error = BoxError,
                Future = Either<
                    Either<
                        BoxFuture<'static, Result<subgraph::Response, BoxError>>,
                        Either<
                            BoxFuture<'static, Result<subgraph::Response, BoxError>>,
                            timeout::future::ResponseFuture<
                                Oneshot<
                                    Either<
                                        Retry<
                                            RetryPolicy,
                                            Either<
                                                rate::service::RateLimit<SubgraphService>,
                                                SubgraphService,
                                            >,
                                        >,
                                        Either<
                                            rate::service::RateLimit<SubgraphService>,
                                            SubgraphService,
                                        >,
                                    >,
                                    subgraph::Request,
                                >,
                            >,
                        >,
                    >,
                    <SubgraphService as Service<subgraph::Request>>::Future,
                >,
            > + Clone
            + Send
            + Sync
            + 'static,
    >,
    BoxError,
> {
    let tls_root_store: Option<RootCertStore> = configuration
        .tls
        .subgraph
        .all
        .create_certificate_store()
        .transpose()?;

    let subscription_plugin_conf = plugins
        .iter()
        .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
        .map(|p| p.config.clone());

    let shaping = plugins
        .iter()
        .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<TrafficShaping>())
        .expect("traffic shaping should always be part of the plugin list");

    let mut subgraph_services = IndexMap::new();
    for (name, _) in schema.subgraphs() {
        let subgraph_service = shaping.subgraph_service_internal(
            name,
            SubgraphService::from_config(
                name,
                configuration,
                &tls_root_store,
                shaping.enable_subgraph_http2(name),
                subscription_plugin_conf.clone(),
            )?,
        );
        subgraph_services.insert(name.clone(), subgraph_service);
    }

    Ok(subgraph_services)
}

impl YamlRouterFactory {
    pub(crate) async fn create_supergraph<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: String,
        previous_router: Option<&'a SupergraphCreator>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    ) -> Result<SupergraphCreator, BoxError> {
        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner = match previous_router.as_ref().map(|router| router.planner()) {
            None => BridgeQueryPlanner::new(schema.clone(), configuration.clone()).await?,
            Some(planner) => {
                BridgeQueryPlanner::new_from_planner(planner, schema.clone(), configuration.clone())
                    .await?
            }
        };

        let schema = bridge_query_planner.schema();

        // Process the plugins.
        let plugins = create_plugins(&configuration, &schema, extra_plugins).await?;

        let mut builder = PluggableSupergraphServiceBuilder::new(bridge_query_planner);
        builder = builder.with_configuration(configuration.clone());
        let subgraph_services = create_subgraph_services(&plugins, &schema, &configuration).await?;
        for (name, subgraph_service) in subgraph_services {
            builder = builder.with_subgraph_service(&name, subgraph_service);
        }
        for (plugin_name, plugin) in plugins {
            builder = builder.with_dyn_plugin(plugin_name, plugin);
        }

        builder.build().await.map_err(BoxError::from)
    }
}

impl TlsSubgraph {
    pub(crate) fn create_certificate_store(
        &self,
    ) -> Option<Result<RootCertStore, ConfigurationError>> {
        self.certificate_authorities
            .as_deref()
            .map(create_certificate_store)
    }
}

pub(crate) fn create_certificate_store(
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

    let service = YamlRouterFactory
        .create(Arc::new(config), schema.to_string(), None, None)
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
    let mut apollo_plugins_config = configuration.apollo_plugins.clone().plugins;
    let user_plugins_config = configuration.plugins.clone().plugins.unwrap_or_default();
    let extra = extra_plugins.unwrap_or_default();
    let plugin_registry = &*crate::plugin::PLUGINS;
    let mut apollo_plugin_factories: HashMap<&str, &PluginFactory> = plugin_registry
        .iter()
        .filter(|factory| factory.name.starts_with(APOLLO_PLUGIN_PREFIX))
        .map(|factory| (factory.name.as_str(), &**factory))
        .collect();
    let mut errors = Vec::new();
    let mut plugin_instances = Vec::new();

    // Use fonction-like macros to avoid borrow conflicts of captures
    macro_rules! add_plugin {
        ($name: expr, $factory: expr, $plugin_config: expr) => {{
            match $factory
                .create_instance(
                    &$plugin_config,
                    schema.as_string().clone(),
                    configuration.notify.clone(),
                )
                .await
            {
                Ok(plugin) => plugin_instances.push(($name, plugin)),
                Err(err) => errors.push(ConfigurationError::PluginConfiguration {
                    plugin: $name,
                    error: err.to_string(),
                }),
            }
        }};
    }

    macro_rules! add_apollo_plugin {
        ($name: literal, $opt_plugin_config: expr) => {{
            let name = format!("{}{}", APOLLO_PLUGIN_PREFIX, $name);
            let factory = apollo_plugin_factories
                .remove(name.as_str())
                .unwrap_or_else(|| panic!("Apollo plugin not registered: {name}"));
            if let Some(mut plugin_config) = $opt_plugin_config {
                if name == "apollo.telemetry" {
                    // The apollo.telemetry" plugin isn't happy with empty config, so we
                    // give it some. If any of the other mandatory plugins need special
                    // treatment, then we'll have to perform it here.
                    // This is *required* by the telemetry module or it will fail...
                    inject_schema_id(schema, &mut plugin_config);
                }
                add_plugin!(name, factory, plugin_config);
            }
        }};
    }

    macro_rules! add_mandatory_apollo_plugin {
        ($name: literal) => {
            add_apollo_plugin!(
                $name,
                Some(
                    apollo_plugins_config
                        .remove($name)
                        .unwrap_or(Value::Object(Map::new()))
                )
            );
        };
    }

    macro_rules! add_optional_apollo_plugin {
        ($name: literal) => {
            add_apollo_plugin!($name, apollo_plugins_config.remove($name));
        };
    }

    macro_rules! add_user_plugins {
        () => {
            for (name, plugin_config) in user_plugins_config {
                if let Some(factory) = plugin_registry.iter().find(|factory| factory.name == name) {
                    add_plugin!(name, factory, plugin_config);
                } else {
                    errors.push(ConfigurationError::PluginUnknown(name))
                }
            }
            plugin_instances.extend(extra);
        };
    }

    add_mandatory_apollo_plugin!("include_subgraph_errors");
    add_mandatory_apollo_plugin!("csrf");
    add_mandatory_apollo_plugin!("headers");
    add_mandatory_apollo_plugin!("telemetry");
    add_mandatory_apollo_plugin!("traffic_shaping");
    add_optional_apollo_plugin!("forbid_mutations");
    add_optional_apollo_plugin!("subscription");
    add_optional_apollo_plugin!("override_subgraph_url");
    add_optional_apollo_plugin!("authorization");
    add_optional_apollo_plugin!("authentication");

    // This relative ordering is documented in `docs/source/customizations/native.mdx`:
    add_optional_apollo_plugin!("rhai");
    add_optional_apollo_plugin!("coprocessor");
    add_user_plugins!();

    // Macros above remove from `apollo_plugin_factories`, so anything left at the end
    // indicates a missing macro call.
    let unused_apollo_plugin_names = apollo_plugin_factories.keys().copied().collect::<Vec<_>>();
    if !unused_apollo_plugin_names.is_empty() {
        panic!(
            "Apollo plugins without their ordering specified in `fn create_plugins`: {}",
            unused_apollo_plugin_names.join(", ")
        )
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
        // Warning: this must be done here, otherwise studio reporting will not work
        if apollo_key().is_some() && apollo_graph_reference().is_some() {
            if let Some(telemetry) = configuration.as_object_mut() {
                telemetry.insert("apollo".to_string(), Value::Object(Default::default()));
            }
        } else {
            return;
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
        "test",
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

    register_plugin!("test", "always_fails_to_start", AlwaysFailsToStartPlugin);

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
                test.always_starts_and_stops:
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
                test.always_fails_to_start:
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
                test.always_starts_and_stops:
                    name: albert
                test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema = include_str!("testdata/supergraph.graphql");

        let service = YamlRouterFactory
            .create(Arc::new(config), schema.to_string(), None, None)
            .await;
        service.map(|_| ())
    }

    #[test]
    fn test_inject_schema_id() {
        let schema = include_str!("testdata/starstuff@current.graphql");
        let schema = Schema::parse_test(schema, &Default::default()).unwrap();
        let mut config = json!({ "apollo": {} });
        inject_schema_id(&schema, &mut config);
        let config =
            serde_json::from_value::<crate::plugins::telemetry::config::Conf>(config).unwrap();
        assert_eq!(
            &config.apollo.unwrap().schema_id,
            "ba573b479c8b3fa273f439b26b9eda700152341d897f18090d52cd073b15f909"
        );
    }
}
