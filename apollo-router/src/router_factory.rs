use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use axum::response::IntoResponse;
use http::StatusCode;
use indexmap::IndexMap;
use multimap::MultiMap;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceExt;
use tower::service_fn;
use tower_service::Service;
use tracing::Instrument;

use crate::AllowedFeature;
use crate::ListenAddr;
use crate::configuration::APOLLO_PLUGIN_PREFIX;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::configuration::TlsClient;
use crate::plugin::DynPlugin;
use crate::plugin::Handler;
use crate::plugin::PluginFactory;
use crate::plugin::PluginInit;
use crate::plugins::subscription::notification::Notify;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::query_planner::QueryPlannerService;
use crate::services::HasSchema;
use crate::services::PluggableSupergraphServiceBuilder;
use crate::services::Plugins;
use crate::services::SubgraphService;
use crate::services::SupergraphCreator;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::http::HttpClientServiceFactory;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineRef;
use crate::services::router::service::RouterCreator;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

pub(crate) const STARTING_SPAN_NAME: &str = "starting";

#[derive(Clone)]
/// A path and a handler to be exposed as a web_endpoint for plugins
pub struct Endpoint {
    pub(crate) path: String,
    // Plugins need to be Send + Sync
    // BoxCloneService isn't enough
    handler: EndpointHandler,
}

#[derive(Clone)]
enum EndpointHandler {
    /// Legacy handler wrapping a router service
    Service(Handler),
    /// Direct axum router (bypasses service conversion)
    Router(axum::Router),
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
    pub fn from_router_service(path: String, handler: router::BoxService) -> Self {
        Self {
            path,
            handler: EndpointHandler::Service(Handler::new(handler)),
        }
    }

    /// Creates an Endpoint given a path and an axum Router
    ///
    /// This is the preferred method for plugins that use axum internally,
    /// as it avoids unnecessary service wrapping and path manipulation.
    ///
    /// The router will be automatically nested at the specified path, allowing
    /// it to handle all sub-routes. For example, a router registered at `/diagnostics`
    /// will handle `/diagnostics/`, `/diagnostics/memory/status`, etc.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axum::{Router, routing::get};
    ///
    /// let router = Router::new()
    ///     .route("/", get(handle_dashboard))
    ///     .route("/status", get(handle_status));
    ///
    /// let endpoint = Endpoint::from_router("/diagnostics".to_string(), router);
    /// // This will handle:
    /// // - /diagnostics/
    /// // - /diagnostics/status
    /// ```
    pub(crate) fn from_router(path: String, router: axum::Router) -> Self {
        Self {
            path,
            handler: EndpointHandler::Router(router),
        }
    }

    pub(crate) fn into_router(self) -> axum::Router {
        match self.handler {
            // If we already have a router, just nest it at the path
            EndpointHandler::Router(router) => axum::Router::new().nest(&self.path, router),
            // Legacy service handling with path-based routing
            EndpointHandler::Service(handler) => {
                let handler_clone = handler.clone();
                let handler = move |req: http::Request<axum::body::Body>| {
                    let endpoint = handler_clone.clone();
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

    fn pipeline_ref(&self) -> Arc<PipelineRef>;
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
        is_telemetry_disabled: bool,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
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
        _is_telemetry_disabled: bool,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
    ) -> Result<Self::RouterFactory, BoxError> {
        // we have to create a telemetry plugin before creating everything else, to generate a trace
        // of router and plugin creation
        let plugin_registry = &*crate::plugin::PLUGINS;
        let mut initial_telemetry_plugin = None;

        if previous_router.is_none()
            && apollo_opentelemetry_initialized()
            && let Some(factory) = plugin_registry
                .iter()
                .find(|factory| factory.name == "apollo.telemetry")
        {
            let mut telemetry_config = configuration
                .apollo_plugins
                .plugins
                .get("telemetry")
                .cloned();
            if let Some(plugin_config) = &mut telemetry_config {
                inject_schema_id(schema.schema_id.as_str(), plugin_config);
                // Extract previous telemetry config for hot reload comparison
                let previous_telemetry_config = previous_router.and_then(|router| {
                    router
                        .configuration
                        .apollo_plugins
                        .plugins
                        .get("telemetry")
                        .cloned()
                });

                let telemetry_init = PluginInit::builder()
                    .config(plugin_config.clone())
                    .and_previous_config(previous_telemetry_config)
                    .supergraph_sdl(schema.raw_sdl.clone())
                    .supergraph_schema_id(schema.schema_id.clone().into_inner())
                    .supergraph_schema(Arc::new(schema.supergraph_schema().clone()))
                    .notify(configuration.notify.clone())
                    .license(license.clone())
                    .full_config(configuration.validated_yaml.clone())
                    .and_original_config_yaml(configuration.raw_yaml.clone())
                    .build();

                match factory.create_instance(telemetry_init).await {
                    Ok(plugin) => {
                        if let Some(telemetry) = plugin
                            .as_any()
                            .downcast_ref::<crate::plugins::telemetry::Telemetry>()
                        {
                            telemetry.activate();
                        }
                        initial_telemetry_plugin = Some(plugin);
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        let router_span = tracing::info_span!(STARTING_SPAN_NAME);
        Self.inner_create(
            configuration,
            schema,
            previous_router,
            initial_telemetry_plugin,
            extra_plugins,
            license,
        )
        .instrument(router_span)
        .await
    }
}

impl YamlRouterFactory {
    async fn inner_create<'a>(
        &'a mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<&'a RouterCreator>,
        initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
    ) -> Result<RouterCreator, BoxError> {
        let mut supergraph_creator = self
            .inner_create_supergraph(
                configuration.clone(),
                schema,
                initial_telemetry_plugin,
                extra_plugins,
                license,
                previous_router,
            )
            .await?;

        // Instantiate the parser here so we can use it to warm up the planner below
        let query_analysis_layer =
            QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&configuration)).await;

        let persisted_query_layer = Arc::new(PersistedQueryLayer::new(&configuration).await?);

        if let Some(previous_router) = previous_router {
            let previous_cache = previous_router.previous_cache();

            supergraph_creator
                .warm_up_query_planner(
                    &query_analysis_layer,
                    &persisted_query_layer,
                    Some(previous_cache),
                    configuration.supergraph.query_planning.warmed_up_queries,
                    configuration
                        .supergraph
                        .query_planning
                        .experimental_reuse_query_plans,
                    &configuration
                        .persisted_queries
                        .experimental_prewarm_query_plan_cache,
                )
                .await;
        } else {
            supergraph_creator
                .warm_up_query_planner(
                    &query_analysis_layer,
                    &persisted_query_layer,
                    None,
                    configuration.supergraph.query_planning.warmed_up_queries,
                    configuration
                        .supergraph
                        .query_planning
                        .experimental_reuse_query_plans,
                    &configuration
                        .persisted_queries
                        .experimental_prewarm_query_plan_cache,
                )
                .await;
        };
        RouterCreator::new(
            query_analysis_layer,
            persisted_query_layer,
            Arc::new(supergraph_creator),
            configuration,
        )
        .await
    }

    pub(crate) async fn inner_create_supergraph(
        &mut self,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
        previous_router: Option<&crate::services::router::service::RouterCreator>,
    ) -> Result<SupergraphCreator, BoxError> {
        let query_planner_span = tracing::info_span!("query_planner_creation");
        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let planner = QueryPlannerService::new(schema.clone(), configuration.clone())
            .instrument(query_planner_span)
            .await?;

        let span = tracing::info_span!("plugins");

        // Process the plugins.
        let subgraph_schemas = Arc::new(
            planner
                .subgraph_schemas()
                .iter()
                .map(|(k, v)| (k.clone(), v.schema.clone()))
                .collect(),
        );

        let plugins: Arc<Plugins> = Arc::new(
            create_plugins(
                &configuration,
                &schema,
                subgraph_schemas,
                initial_telemetry_plugin,
                extra_plugins,
                license,
                previous_router,
            )
            .instrument(span)
            .await?
            .into_iter()
            .collect(),
        );

        async {
            let mut builder = PluggableSupergraphServiceBuilder::new(planner);
            builder = builder.with_configuration(configuration.clone());
            let http_service_factory =
                create_http_services(&plugins, &schema, &configuration).await?;
            let subgraph_services =
                create_subgraph_services(&http_service_factory, &configuration).await?;
            builder = builder.with_http_service_factory(http_service_factory);
            for (name, subgraph_service) in subgraph_services {
                builder = builder.with_subgraph_service(&name, subgraph_service);
            }

            // Final creation after this line we must NOT fail to go live with the new router from this point as some plugins may interact with globals.
            let supergraph_creator = builder.with_plugins(plugins).build().await?;

            Ok(supergraph_creator)
        }
        .instrument(tracing::info_span!("supergraph_creation"))
        .await
    }
}

pub(crate) async fn create_subgraph_services(
    http_service_factory: &IndexMap<String, HttpClientServiceFactory>,
    configuration: &Configuration,
) -> Result<IndexMap<String, SubgraphService>, BoxError> {
    let mut subgraph_services = IndexMap::default();
    for (name, http_service_factory) in http_service_factory.iter() {
        let subgraph_service = SubgraphService::from_config(
            name.clone(),
            configuration,
            http_service_factory.clone(),
        )?;
        subgraph_services.insert(name.clone(), subgraph_service);
    }

    Ok(subgraph_services)
}

pub(crate) async fn create_http_services(
    plugins: &Arc<Plugins>,
    schema: &Schema,
    configuration: &Configuration,
) -> Result<IndexMap<String, HttpClientServiceFactory>, BoxError> {
    // Note we are grabbing these root stores once and then reusing it for each subgraph. Why?
    // When TLS was not configured for subgraphs, the OS provided list of certificates was parsed once per subgraph, which resulted in long loading times on OSX.
    // This generates the native root store once, and reuses it across subgraphs
    let subgraph_tls_root_store: RootCertStore = configuration
        .tls
        .subgraph
        .all
        .create_certificate_store()
        .transpose()?
        .unwrap_or_else(crate::services::http::HttpClientService::native_roots_store);
    let connector_tls_root_store: RootCertStore = configuration
        .tls
        .connector
        .all
        .create_certificate_store()
        .transpose()?
        .unwrap_or_else(crate::services::http::HttpClientService::native_roots_store);

    let shaping = plugins
        .iter()
        .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<TrafficShaping>())
        .expect("traffic shaping should always be part of the plugin list");

    let connector_subgraphs: HashSet<String> = schema
        .connectors
        .as_ref()
        .map(|c| {
            c.by_service_name
                .iter()
                .map(|(_, connector)| connector.id.subgraph_name.clone())
                .collect()
        })
        .unwrap_or_default();

    let mut http_services = IndexMap::new();
    for (name, _) in schema.subgraphs() {
        if connector_subgraphs.contains(name) {
            continue; // Avoid adding services for subgraphs that are actually connectors since we'll separately add them below per source
        }
        let http_service = crate::services::http::HttpClientService::from_config_for_subgraph(
            name,
            configuration,
            &subgraph_tls_root_store,
            shaping.subgraph_client_config(name),
        )?;

        let http_service_factory = HttpClientServiceFactory::new(http_service, plugins.clone());
        http_services.insert(name.clone(), http_service_factory);
    }

    // Also create client service factories for connector sources
    let connector_sources = schema
        .connectors
        .as_ref()
        .map(|c| c.source_config_keys.clone())
        .unwrap_or_default();

    for name in connector_sources.iter() {
        let http_service = crate::services::http::HttpClientService::from_config_for_connector(
            name,
            configuration,
            &connector_tls_root_store,
            shaping.connector_client_config(name),
        )?;

        let http_service_factory = HttpClientServiceFactory::new(http_service, plugins.clone());
        http_services.insert(name.clone(), http_service_factory);
    }

    Ok(http_services)
}

impl TlsClient {
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
            .add(certificate)
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

fn load_certs(certificates: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    tracing::debug!("loading root certificates");

    // Load and return certificate.
    rustls_pemfile::certs(&mut certificates.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        // XXX(@goto-bus-stop): the error type here is already io::Error. Should we wrap it,
        // instead of replacing it with this generic error message?
        .map_err(|_| io::Error::other("failed to load certificate"))
}

/// test only helper method to create a router factory in integration tests
///
/// not meant to be used directly
pub async fn create_test_service_factory_from_yaml(schema: &str, configuration: &str) {
    let config: Configuration = serde_yaml::from_str(configuration).unwrap();
    let schema = Arc::new(Schema::parse(schema, &config).unwrap());

    let is_telemetry_disabled = false;
    let service = YamlRouterFactory
        .create(
            is_telemetry_disabled,
            Arc::new(config),
            schema,
            None,
            None,
            Default::default(),
        )
        .await;
    assert_eq!(
        service.map(|_| ()).unwrap_err().to_string().as_str(),
        r#"failed to initialize the query planner: An internal error has occurred, please report this bug to Apollo.

Details: Object field "Product.reviews"'s inner type "Review" does not refer to an existing output type."#
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_plugin(
    name: String,
    factory: &PluginFactory,
    plugin_config: &Value,
    previous_plugin_config: Option<&Value>,
    schema: Arc<String>,
    schema_id: Arc<String>,
    supergraph_schema: Arc<Valid<apollo_compiler::Schema>>,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    launch_id: Option<Arc<String>>,
    notify: &Notify<String, crate::graphql::Response>,
    plugin_instances: &mut Plugins,
    errors: &mut Vec<ConfigurationError>,
    license: Arc<LicenseState>,
    full_config: Option<Value>,
    original_config_yaml: Option<Arc<str>>,
) {
    let plugin_init = PluginInit::builder()
        .config(plugin_config.clone())
        .and_previous_config(previous_plugin_config.cloned())
        .supergraph_sdl(schema)
        .supergraph_schema_id(schema_id)
        .supergraph_schema(supergraph_schema)
        .subgraph_schemas(subgraph_schemas)
        .launch_id(launch_id)
        .notify(notify.clone())
        .license(license)
        .and_full_config(full_config)
        .and_original_config_yaml(original_config_yaml)
        .build();

    match factory.create_instance(plugin_init).await {
        Ok(plugin) => {
            let _ = plugin_instances.insert(name, plugin);
        }
        Err(err) => errors.push(ConfigurationError::PluginConfiguration {
            plugin: name,
            error: err.to_string(),
        }),
    }
}

pub(crate) async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
    subgraph_schemas: Arc<HashMap<String, Arc<Valid<apollo_compiler::Schema>>>>,
    initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
    previous_router: Option<&crate::services::router::service::RouterCreator>,
) -> Result<Plugins, BoxError> {
    let supergraph_schema = Arc::new(schema.supergraph_schema().clone());
    let supergraph_schema_id = schema.schema_id.clone().into_inner();
    let mut apollo_plugins_config = configuration.apollo_plugins.clone().plugins;
    let user_plugins_config = configuration.plugins.clone().plugins.unwrap_or_default();

    // Extract previous plugin configurations for hot reload previous config detection
    let (previous_apollo_plugins_config, previous_user_plugins_config) = match previous_router {
        Some(router) => {
            // Extract apollo plugin configs from the previous router's stored configuration
            let prev_apollo_configs: HashMap<&str, &Value> = router
                .configuration
                .apollo_plugins
                .plugins
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect();

            // Extract user plugin configs from the previous router's stored configuration
            let prev_user_configs: HashMap<String, &Value> = router
                .configuration
                .plugins
                .plugins
                .as_ref()
                .map(|plugins| plugins.iter().map(|(k, v)| (k.clone(), v)).collect())
                .unwrap_or_default();

            (prev_apollo_configs, prev_user_configs)
        }
        None => (HashMap::new(), HashMap::new()),
    };
    let extra = extra_plugins.unwrap_or_default();
    let plugin_registry = &*crate::plugin::PLUGINS;
    let apollo_telemetry_plugin_mandatory = apollo_opentelemetry_initialized();
    let mut apollo_plugin_factories: HashMap<&str, &PluginFactory> = plugin_registry
        .iter()
        .filter(|factory| {
            // the name starts with apollo
            factory.name.starts_with(APOLLO_PLUGIN_PREFIX)
                && (
                    // the plugin is mandatory
                    apollo_telemetry_plugin_mandatory ||
                    // the name isn't apollo.telemetry
                    factory.name != "apollo.telemetry"
                )
        })
        .map(|factory| (factory.name.as_str(), &**factory))
        .collect();
    let mut errors = Vec::new();
    let mut plugin_instances = Plugins::default();

    // Use function-like macros to avoid borrow conflicts of captures
    macro_rules! add_plugin {
        ($name: expr, $factory: expr, $plugin_config: expr, $maybe_full_config: expr, $previous_plugin_config: expr) => {{
            add_plugin(
                $name,
                $factory,
                &$plugin_config,
                $previous_plugin_config,
                schema.as_string().clone(),
                supergraph_schema_id.clone(),
                supergraph_schema.clone(),
                subgraph_schemas.clone(),
                schema.launch_id.clone(),
                &configuration.notify.clone(),
                &mut plugin_instances,
                &mut errors,
                license.clone(),
                $maybe_full_config,
                configuration.raw_yaml.clone(),
            )
            .await;
        }};
    }

    macro_rules! add_mandatory_apollo_plugin_inner {
        ($name: literal, $opt_plugin_config: expr) => {{
            let name = concat!("apollo.", $name);
            let span = tracing::info_span!(concat!("plugin: ", "apollo.", $name));
            async {
                let factory = apollo_plugin_factories
                    .remove(name)
                    .unwrap_or_else(|| panic!("Apollo plugin not registered: {name}"));
                if let Some(mut plugin_config) = $opt_plugin_config {
                    let mut full_config = None;
                    if name == "apollo.telemetry" {
                        // The apollo.telemetry" plugin isn't happy with empty config, so we
                        // give it some. If any of the other mandatory plugins need special
                        // treatment, then we'll have to perform it here
                        inject_schema_id(&supergraph_schema_id, &mut plugin_config);

                        // Only the telemetry plugin should have access to the full configuration
                        full_config = configuration.validated_yaml.clone();
                    }
                    let previous_config = previous_apollo_plugins_config.get($name).copied();
                    add_plugin!(
                        name.to_string(),
                        factory,
                        plugin_config,
                        full_config,
                        previous_config
                    );
                }
            }
            .instrument(span)
            .await;
        }};
    }

    macro_rules! add_optional_apollo_plugin_inner {
        ($name: literal, $opt_plugin_config: expr, $license: expr) => {{
            let name = concat!("apollo.", $name);
            let span = tracing::info_span!(concat!("plugin: ", "apollo.", $name));
            async {
                let factory = apollo_plugin_factories
                    .remove(name)
                    .unwrap_or_else(|| panic!("Apollo plugin not registered: {name}"));
                if let Some(plugin_config) = $opt_plugin_config {
                    let allowed_features = $license.get_allowed_features();

                    match AllowedFeature::from_plugin_name($name) {
                        Some(allowed_feature) => {
                            if allowed_features.contains(&allowed_feature) {
                                let previous_config = previous_apollo_plugins_config.get($name).copied();
                                add_plugin!(name.to_string(), factory, plugin_config, None, previous_config);
                            } else {
                                tracing::warn!(
                                    "{name} plugin is not registered, {name} is a restricted feature that requires a license"
                                );
                            }
                        }
                        None => {
                            // If the plugin name did not map to an allowed feature we add it
                            let previous_config = previous_apollo_plugins_config.get($name).copied();
                            add_plugin!(name.to_string(), factory, plugin_config, None, previous_config);
                        }
                    }
                }
            }
            .instrument(span)
            .await;
        }};
    }

    macro_rules! add_oss_apollo_plugin_inner {
        ($name: literal, $opt_plugin_config: expr) => {{
            let name = concat!("apollo.", $name);
            let span = tracing::info_span!(concat!("plugin: ", "apollo.", $name));
            async {
                let factory = apollo_plugin_factories
                    .remove(name)
                    .unwrap_or_else(|| panic!("Apollo plugin not registered: {name}"));
                if let Some(plugin_config) = $opt_plugin_config {
                    // We add oss plugins without a license check
                    let previous_config = previous_apollo_plugins_config.get($name).copied();
                    add_plugin!(
                        name.to_string(),
                        factory,
                        plugin_config,
                        None,
                        previous_config
                    );
                    return;
                }
            }
            .instrument(span)
            .await;
        }};
    }

    macro_rules! add_mandatory_apollo_plugin {
        ($name: literal) => {
            add_mandatory_apollo_plugin_inner!(
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
            add_optional_apollo_plugin_inner!($name, apollo_plugins_config.remove($name), &license);
        };
    }

    macro_rules! add_oss_apollo_plugin {
        ($name: literal) => {
            add_oss_apollo_plugin_inner!($name, apollo_plugins_config.remove($name));
        };
    }

    macro_rules! add_user_plugins {
        () => {
            for (name, plugin_config) in user_plugins_config {
                let user_span = tracing::info_span!("user_plugin", "name" = &name);

                async {
                    if let Some(factory) =
                        plugin_registry.iter().find(|factory| factory.name == name)
                    {
                        let previous_config = previous_user_plugins_config.get(&name).copied();
                        add_plugin!(name, factory, plugin_config, None, previous_config);
                    } else {
                        errors.push(ConfigurationError::PluginUnknown(name))
                    }
                }
                .instrument(user_span)
                .await;
            }

            plugin_instances.extend(extra);
        };
    }

    // Be careful with this list! Moving things around can have subtle consequences.
    // Requests flow through this list multiple times in two directions. First, they go "down"
    // through the list several times as requests at the different services. Then, they go
    // "up" through the list as a response several times, once for each service.
    //
    // The order of this list determines the relative order of plugin hooks executing at each
    // service. This is *not* the same as the order a request flows through the router.
    // For example, assume these three plugins:
    // 1. header propagation (has a hook at the subgraph service)
    // 2. telemetry (has hooks at router, supergraph, and subgraph services)
    // 3. rate limiting (has a hook at the router service)
    // The order here means that header propagation happens before telemetry *at the subgraph
    // service*. Depending on the requirements of plugins, it may have to be in this order. The
    // *router service* hook for telemetry still happens well before header propagation. Similarly,
    // header propagation being first does not mean that it's exempt from rate limiting, for the
    // same reason. Rate limiting must be after telemetry, though, because telemetry and rate
    // limiting both work at the router service, and requests rejected from the router service must
    // flow through telemetry so we can record errors.
    //
    // Broadly, for telemetry to work, we must make sure that the telemetry plugin is the first
    // plugin in this list *that adds a router service hook*. Other plugins can be before the
    // telemetry plugin if they must do work *before* telemetry at specific services.
    add_mandatory_apollo_plugin!("include_subgraph_errors");
    add_mandatory_apollo_plugin!("headers");
    if apollo_telemetry_plugin_mandatory {
        match initial_telemetry_plugin {
            None => {
                add_mandatory_apollo_plugin!("telemetry");
            }
            Some(plugin) => {
                let _ = plugin_instances.insert("apollo.telemetry".to_string(), plugin);
                apollo_plugins_config.remove("apollo.telemetry");
                apollo_plugin_factories.remove("apollo.telemetry");
            }
        }
    }
    add_mandatory_apollo_plugin!("license_enforcement");
    add_mandatory_apollo_plugin!("health_check");
    add_mandatory_apollo_plugin!("traffic_shaping");
    add_mandatory_apollo_plugin!("limits");
    add_mandatory_apollo_plugin!("csrf");
    add_mandatory_apollo_plugin!("fleet_detector");
    add_mandatory_apollo_plugin!("enhanced_client_awareness");
    add_mandatory_apollo_plugin!("experimental_diagnostics");

    add_oss_apollo_plugin!("forbid_mutations");
    add_optional_apollo_plugin!("subscription");
    add_oss_apollo_plugin!("override_subgraph_url");
    add_optional_apollo_plugin!("authorization");
    add_optional_apollo_plugin!("authentication");
    add_oss_apollo_plugin!("preview_file_uploads");
    add_optional_apollo_plugin!("preview_entity_cache");
    add_mandatory_apollo_plugin!("progressive_override");
    add_optional_apollo_plugin!("demand_control");

    // This relative ordering is documented in `docs/source/customizations/native.mdx`:
    add_oss_apollo_plugin!("connectors");
    add_oss_apollo_plugin!("rhai");
    add_optional_apollo_plugin!("coprocessor");
    add_optional_apollo_plugin!("preview_response_cache");
    add_user_plugins!();

    // Because this plugin intercepts subgraph requests
    // and does not forward them to the next service in the chain,
    // it needs to intervene after user plugins for users plugins to run at all.
    add_optional_apollo_plugin!("experimental_mock_subgraphs");

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

        let errors_list = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
            .join("\n");

        Err(BoxError::from(format!(
            "there were {} configuration errors\n{}",
            errors.len(),
            errors_list
        )))
    } else {
        Ok(plugin_instances)
    }
}

fn inject_schema_id(
    // Ideally we'd use &SchemaHash, but we'll need to update a bunch of tests to do so
    schema_id: &str,
    configuration: &mut Value,
) {
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
    if let Some(apollo) = configuration.get_mut("apollo")
        && let Some(apollo) = apollo.as_object_mut()
    {
        apollo.insert(
            "schema_id".to_string(),
            Value::String(schema_id.to_string()),
        );
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::sync::Arc;

    use rstest::rstest;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;
    use tower_http::BoxError;

    use crate::AllowedFeature;
    use crate::configuration::Configuration;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::register_plugin;
    use crate::router_factory::RouterSuperServiceFactory;
    use crate::router_factory::YamlRouterFactory;
    use crate::router_factory::inject_schema_id;
    use crate::services::supergraph::service::HasPlugins;
    use crate::spec::Schema;
    use crate::uplink::license_enforcement::LicenseLimits;
    use crate::uplink::license_enforcement::LicenseState;

    const MANDATORY_PLUGINS: &[&str] = &[
        "apollo.include_subgraph_errors",
        "apollo.headers",
        "apollo.license_enforcement",
        "apollo.health_check",
        "apollo.traffic_shaping",
        "apollo.limits",
        "apollo.csrf",
        "apollo.fleet_detector",
        "apollo.enhanced_client_awareness",
        "apollo.progressive_override",
    ];

    const OSS_PLUGINS: &[&str] = &[
        "apollo.forbid_mutations",
        "apollo.override_subgraph_url",
        "apollo.connectors",
    ];

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

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &config)?;

        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(config),
                Arc::new(schema),
                None,
                None,
                Arc::new(LicenseState::default()),
            )
            .await;
        service.map(|_| ())
    }

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

    #[test]
    fn test_inject_schema_id() {
        let mut config = json!({ "apollo": {} });
        inject_schema_id(
            "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8",
            &mut config,
        );
        let config =
            serde_json::from_value::<crate::plugins::telemetry::config::Conf>(config).unwrap();
        assert_eq!(
            &config.apollo.schema_id,
            "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8"
        );
    }

    fn get_plugin_config(plugin: &str) -> &str {
        match plugin {
            "subscription" => {
                r#"
                enabled: true
                "#
            }
            "authentication" => {
                r#"
                connector:
                  sources: {}
                "#
            }
            "authorization" => {
                r#"
                require_authentication: false
                "#
            }
            "preview_file_uploads" => {
                r#"
                enabled: true
                protocols:
                  multipart:
                    enabled: false
                "#
            }
            "preview_entity_cache" => {
                r#"
                enabled: true
                subgraph:
                  all:
                    enabled: true
                "#
            }
            "preview_response_cache" => {
                r#"
                enabled: true
                subgraph:
                  all:
                    enabled: true
                "#
            }
            "demand_control" => {
                r#"
                enabled: true
                mode: measure
                strategy:
                  static_estimated:
                    list_size: 0
                    max: 0.0
                "#
            }
            "coprocessor" => {
                r#"
                url: http://service.example.com/url
                "#
            }
            "connectors" => {
                r#"
                debug_extensions: false
                "#
            }
            "experimental_mock_subgraphs" => {
                r#"
               subgraphs: {}
                "#
            }
            "forbid_mutations" => {
                r#"
                false
                "#
            }
            "override_subgraph_url" => {
                r#"
                {}
                "#
            }
            _ => panic!("This function does not contain config for plugin: {plugin}"),
        }
    }

    #[tokio::test]
    #[rstest]
    #[case::empty_allowed_features_set(HashSet::new())]
    #[case::nonempty_allowed_features_set(HashSet::from_iter(vec![AllowedFeature::Coprocessors]))]
    async fn test_mandatory_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let router_config = Configuration::builder().build().unwrap();
        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - the mandatory plugins are added
         * */
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::allowed_features_empty(HashSet::new())]
    #[case::allowed_features_nonempty(HashSet::from_iter(vec![
        AllowedFeature::Coprocessors,
        AllowedFeature::DemandControl
    ]))]
    async fn test_oss_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config that contains configuration for oss plugins
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        // Create config for oss plugins
        let forbid_mutations_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations"))
                .unwrap();
        let override_subgraph_url_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
                .unwrap();
        let connectors_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();

        let router_config = Configuration::builder()
            .apollo_plugin("forbid_mutations", forbid_mutations_config)
            .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
            .apollo_plugin("connectors", connectors_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - all oss plugins should have been added
         * */
        assert!(
            OSS_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::subscripions(
        "subscription",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Subscriptions]))
    ]
    #[case::authorization(
        "authorization",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions]))
    ]
    #[case::authentication(
        "authentication",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
    ]
    #[case::entity_caching(
        "preview_entity_cache",
        HashSet::from_iter(vec![AllowedFeature::EntityCaching, AllowedFeature::DemandControl]))
    ]
    #[case::response_cache(
        "preview_response_cache",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::ResponseCaching]))
    ]
    #[case::authorization(
        "demand_control",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::DemandControl]))
    ]
    #[case::coprocessor(
        "coprocessor",
        HashSet::from_iter(vec![AllowedFeature::Coprocessors, AllowedFeature::DemandControl]))
    ]
    async fn test_optional_plugin_added_with_restricted_allowed_features(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a restricted license with allowed feature set containing the given `plugin`
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        dbg!(&plugin_config);
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since the plugin is part of the `allowed_features` set
         *    the plugin should have been added.
         * - mandatory plugins should have been added.
         * */
        assert!(
            service
                .supergraph_creator
                .plugins()
                .contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::subscripions(
        "subscription",
        HashSet::from_iter(vec![]))
    ]
    #[case::authorization(
        "authorization",
        HashSet::from_iter(vec![AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
    ]
    #[case::authentication(
        "authentication",
        HashSet::from_iter(vec![AllowedFeature::DemandControl,AllowedFeature::Subscriptions]))
    ]
    #[case::entity_caching(
        "preview_entity_cache",
        HashSet::from_iter(vec![AllowedFeature::DemandControl]))
    ]
    #[case::response_cache(
        "preview_response_cache",
        HashSet::from_iter(vec![AllowedFeature::EntityCaching]))
    ]
    #[case::authorization(
        "demand_control",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::Experimental]))
    ]
    #[case::coprocessor(
        "coprocessor",
        HashSet::from_iter(vec![AllowedFeature::DemandControl]))
    ]
    async fn test_optional_plugin_not_added_with_restricted_allowed_features(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a restricted license whose allowed feature set does not contain the given `plugin`
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since the plugin is not part of the `allowed_features` set
         *    the plugin should not have been added.
         * - mandatory plugins should have been added.
         * */
        assert!(
            !service
                .supergraph_creator
                .plugins()
                .contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should not have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::mock_subgraphs_non_empty_allowed_features(
        "experimental_mock_subgraphs",
        HashSet::from_iter(vec![AllowedFeature::DemandControl])
    )]
    #[case::mock_subgraphs_empty_allowed_features(
        "experimental_mock_subgraphs",
        HashSet::from_iter(vec![])
    )]
    async fn test_optional_plugin_that_does_not_map_to_an_allowed_feature_is_added(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config including valid config for the optional plugin that does
         *    not map to an allowed feature
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         * - the plugin should be added
         * - mandatory plugins should have been added.
         * - coprocessors and subscritions (both gated features) should not have been added.
         * */
        assert!(
            service
                .supergraph_creator
                .plugins()
                .contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
        // These gated features should not have been added
        assert!(
            !service
                .supergraph_creator
                .plugins()
                .contains_key("apollo.subscription"),
            "Plugin {plugin} should not have been added"
        );
        assert!(
            !service
                .supergraph_creator
                .plugins()
                .contains_key("apollo.coprocessor"),
            "Plugin {plugin} should not have been added"
        );
    }

    #[tokio::test]
    #[rstest]
    // NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
    #[case::forbid_mutations("forbid_mutations")]
    #[case::subscriptions("subscription")]
    #[case::override_subgraph_url("override_subgraph_url")]
    #[case::authorization("authorization")]
    #[case::authentication("authentication")]
    #[case::file_upload("preview_file_uploads")]
    #[case::entity_cache("preview_entity_cache")]
    #[case::response_cache("preview_response_cache")]
    #[case::demand_control("demand_control")]
    #[case::connectors("connectors")]
    #[case::coprocessor("coprocessor")]
    #[case::mock_subgraphs("experimental_mock_subgraphs")]
    async fn test_optional_plugin_with_unrestricted_allowed_features(#[case] plugin: &str) {
        /*
         * GIVEN
         *  - a license with unrestricted limits (includes allowing all features)
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Default::default(),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since `allowed_features` is unrestricted plugin should have been added.
         * */
        assert!(
            service
                .supergraph_creator
                .plugins()
                .contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    // NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
    #[case::forbid_mutations("forbid_mutations")]
    #[case::subscriptions("subscription")]
    #[case::override_subgraph_url("override_subgraph_url")]
    #[case::authorization("authorization")]
    #[case::authentication("authentication")]
    #[case::file_upload("preview_file_uploads")]
    #[case::response_cache("preview_response_cache")]
    #[case::demand_control("demand_control")]
    #[case::connectors("connectors")]
    #[case::coprocessor("coprocessor")]
    #[case::mock_subgraphs("experimental_mock_subgraphs")]
    async fn test_optional_plugin_with_default_license_limits(#[case] plugin: &str) {
        /*
         * GIVEN
         *  - a license with license limits None
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Default::default(),
        };

        // Create config for the given `plugin`
        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();

        // Create config for oss plugins
        // Create config for oss plugins
        let forbid_mutations_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations"))
                .unwrap();
        let override_subgraph_url_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
                .unwrap();
        let connectors_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();
        let response_cache_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("preview_response_cache"))
                .unwrap();

        let router_config = Configuration::builder()
            .apollo_plugin("forbid_mutations", forbid_mutations_config)
            .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
            .apollo_plugin("connectors", connectors_config)
            .apollo_plugin("preview_response_cache", response_cache_config)
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  // NB: this behavior may change once all licenses have an `allowed_features` claim
         *  - when license limits are None we default to unrestricted allowed features
         *  - the given `plugin` should have been added
         *  - all mandatory plugins should have been added
         *  - all oss plugins in the config should have been added
         * */
        assert!(
            service
                .supergraph_creator
                .plugins()
                .contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
        assert!(
            OSS_PLUGINS
                .iter()
                .all(|plugin| { service.supergraph_creator.plugins().contains_key(*plugin) })
        );
    }
}
