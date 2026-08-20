//! Construction of the router's serving pipeline from configuration, schema, and license.
//!
//! [`build_pipeline`] runs three phases, each under its own tracing span:
//!
//! - **Acquire** gathers every resource whose creation can fail: the telemetry plugin, the
//!   federation query planner, the other plugins, TLS/DNS client material, Redis clients,
//!   and the persisted-query manifest.
//! - **Activate** runs every plugin's `activate()` hook. The telemetry hook swaps in global
//!   tracer and meter providers that cannot be rolled back. Nothing after this phase starts
//!   may fail.
//! - **Assemble** builds the caches and service stacks from the acquired resources, using
//!   infallible functions. Each cache registers its gauges in its constructor; constructing
//!   caches after the meter-provider swap binds those gauges to the provider that serves
//!   this pipeline.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::validation::Valid;
use indexmap::IndexMap;
use rustls::RootCertStore;
use serde_json::Map;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;

use crate::AllowedFeature;
use crate::cache::DeduplicatingCache;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::storage::connect_redis;
use crate::configuration::APOLLO_PLUGIN_PREFIX;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::plugin::DynPlugin;
use crate::plugin::PluginFactory;
use crate::plugin::PluginInit;
use crate::plugins::subscription::notification::Notify;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::QueryPlanCache;
use crate::query_planner::QueryPlannerService;
use crate::query_planner::SubgraphSchemas;
use crate::query_planner::warmup;
use crate::services::Plugins;
use crate::services::SubgraphService;
use crate::services::SupergraphCreator;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::build_supergraph_creator;
use crate::services::http::HttpClientService;
use crate::services::http::HttpClientServiceFactory;
use crate::services::http::service::HttpClientMaterial;
use crate::services::layers::apq::APQExpander;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::query_parsing;
use crate::services::query_planner;
use crate::services::router::service::RouterCreator;
use crate::services::subgraph;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Builds a serving pipeline from configuration, schema, and license.
///
/// Runs the acquire, activate, and assemble phases the module docs describe. On a hot
/// reload, pass the previous pipeline's configuration and in-memory query-plan cache:
/// the early telemetry activation is then skipped and warm-up replays the previously
/// cached queries against the new planner.
pub(crate) async fn build_pipeline(
    configuration: Arc<Configuration>,
    schema: Arc<Schema>,
    previous_config: Option<Arc<Configuration>>,
    previous_cache: Option<InMemoryQueryPlanCache>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
) -> Result<RouterCreator, BoxError> {
    let Acquired {
        query_planner_service,
        subgraph_schemas,
        plugins,
        subgraph_client_material,
        connector_client_material,
        query_plan_redis,
        apq_redis,
        persisted_queries,
    } = acquire(
        &configuration,
        &schema,
        previous_config,
        extra_plugins,
        license,
    )
    .instrument(tracing::info_span!("acquire"))
    .await?;

    {
        // The point of no return: activating the telemetry plugin swaps in global tracer
        // and meter providers that cannot be rolled back. From here on the pipeline must
        // go live, which is why the assemble phase below is infallible.
        let _span = tracing::info_span!("activate").entered();
        for (_, plugin) in plugins.iter() {
            plugin.activate();
        }
    }

    let router_creator = async {
        let query_plan_cache = build_query_plan_cache(&configuration, query_plan_redis);
        let apq_expander = build_apq_expander(&configuration, apq_redis);
        let query_parsing_service =
            query_parsing::query_parsing_service(schema.clone(), configuration.clone());

        let (supergraph_creator, caching_query_planner) = {
            let _span = tracing::info_span!("supergraph_creation").entered();
            let (http_service_factory, connector_http_service_factory) = build_http_services(
                subgraph_client_material,
                connector_client_material,
                &plugins,
            );
            let subgraph_services = create_subgraph_services(&http_service_factory);
            build_supergraph_creator(
                query_planner_service,
                query_plan_cache,
                schema.clone(),
                subgraph_schemas,
                configuration.clone(),
                plugins,
                subgraph_services.into_iter().collect(),
                connector_http_service_factory,
            )
        };

        let router_creator = RouterCreator::new(
            persisted_queries.clone(),
            apq_expander,
            Arc::new(supergraph_creator),
            query_parsing_service.clone(),
            configuration.clone(),
        );

        let warmup_query_planner_service = ServiceBuilder::new()
            .layer(warmup::WarmupParseQueryLayer::new(query_parsing_service))
            .map_response(drop) // Ignore response
            .service(caching_query_planner)
            .boxed_clone();

        SupergraphCreator::warm_up_query_planner(
            warmup_query_planner_service,
            &persisted_queries,
            previous_cache,
            configuration.supergraph.query_planning.warmed_up_queries,
            &configuration
                .persisted_queries
                .experimental_prewarm_query_plan_cache,
        )
        .instrument(tracing::info_span!("warmup"))
        .await;

        router_creator
    }
    .instrument(tracing::info_span!("assemble"))
    .await;

    Ok(router_creator)
}

/// Everything the acquire phase produces. Each field except `subgraph_schemas` (an
/// infallible byproduct of planner creation) is a resource whose creation can fail; the
/// assemble phase consumes them infallibly.
struct Acquired {
    query_planner_service: query_planner::BoxCloneService,
    subgraph_schemas: Arc<SubgraphSchemas>,
    plugins: Arc<Plugins>,
    subgraph_client_material: IndexMap<String, HttpClientMaterial>,
    connector_client_material: IndexMap<String, HttpClientMaterial>,
    query_plan_redis: Option<RedisCacheStorage>,
    apq_redis: Option<RedisCacheStorage>,
    persisted_queries: Arc<PersistedQueryExpander>,
}

async fn acquire(
    configuration: &Arc<Configuration>,
    schema: &Arc<Schema>,
    previous_config: Option<Arc<Configuration>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
) -> Result<Acquired, BoxError> {
    let initial_telemetry_plugin =
        init_telemetry(configuration, schema, &license, previous_config.as_deref()).await?;

    let (query_planner_service, subgraph_schemas) =
        create_query_planner_service(schema, configuration)?;

    let plugins: Arc<Plugins> = Arc::new(
        create_plugins(
            configuration,
            schema,
            subgraph_schemas.clone(),
            initial_telemetry_plugin,
            extra_plugins,
            license,
            previous_config,
        )
        .instrument(tracing::info_span!("plugins"))
        .await?
        .into_iter()
        .collect(),
    );

    let (subgraph_client_material, connector_client_material) =
        parse_http_client_material(&plugins, schema, configuration)?;

    let query_plan_redis = connect_query_plan_redis(configuration)
        .instrument(tracing::info_span!("query_plan_redis_connect"))
        .await?;
    let apq_redis = connect_apq_redis(configuration)
        .instrument(tracing::info_span!("apq_redis_connect"))
        .await?;

    let persisted_queries = Arc::new(
        PersistedQueryExpander::new(configuration)
            .instrument(tracing::info_span!("persisted_queries_manifest"))
            .await?,
    );

    Ok(Acquired {
        query_planner_service,
        subgraph_schemas,
        plugins,
        subgraph_client_material,
        connector_client_material,
        query_plan_redis,
        apq_redis,
        persisted_queries,
    })
}

/// Creates and activates the telemetry plugin before the rest of pipeline construction.
///
/// Tracing has to be live before the other plugins and services are built, or none of
/// that construction gets traced. Hand the returned plugin to `create_plugins`, which
/// splices it into the plugin map instead of building telemetry a second time.
///
/// Returns `None` when early activation is unnecessary or impossible:
///
/// - on a hot reload (`previous_config` is `Some`), because the telemetry activated at
///   first boot is still live
/// - when the process never installed the global OpenTelemetry layer
/// - when the configuration has no `telemetry` section
async fn init_telemetry(
    configuration: &Configuration,
    schema: &Schema,
    license: &Arc<LicenseState>,
    previous_config: Option<&Configuration>,
) -> Result<Option<Box<dyn DynPlugin>>, BoxError> {
    let plugin_registry = &*crate::plugin::PLUGINS;
    let mut initial_telemetry_plugin = None;

    if previous_config.is_none()
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
            let previous_telemetry_config = previous_config
                .and_then(|config| config.apollo_plugins.plugins.get("telemetry").cloned());

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

    Ok(initial_telemetry_plugin)
}

/// Creates the federation query planner service and the subgraph schemas it extracts.
///
/// # Errors
/// Fails when the schema uses unsupported federation versions or features, or when its
/// authorization directives do not validate.
pub(crate) fn create_query_planner_service(
    schema: &Arc<Schema>,
    configuration: &Arc<Configuration>,
) -> Result<(query_planner::BoxCloneService, Arc<SubgraphSchemas>), BoxError> {
    let _span = tracing::info_span!("query_planner_creation").entered();

    let planner = QueryPlannerService::create_planner(schema, configuration)?;
    let subgraph_schemas = crate::query_planner::build_subgraph_schemas(&planner);

    let query_planner_service =
        QueryPlannerService::new(schema.clone(), configuration.clone(), planner)?.boxed_clone();

    Ok((query_planner_service, subgraph_schemas))
}

/// `(subgraph_material, connector_material)`: client material for regular subgraphs keyed
/// by subgraph name, and for connector sources keyed by `source_config_key()`
/// (`{subgraph_name}.{source_or_synthetic}`).
pub(crate) type HttpClientMaterialMaps = (
    IndexMap<String, HttpClientMaterial>,
    IndexMap<String, HttpClientMaterial>,
);

/// Parses TLS and DNS client material for every subgraph and connector source.
///
/// This is the fallible half of HTTP client construction; [`build_http_services`] turns
/// the material into services.
pub(crate) fn parse_http_client_material(
    plugins: &Plugins,
    schema: &Schema,
    configuration: &Configuration,
) -> Result<HttpClientMaterialMaps, BoxError> {
    // Build each root store once and share it across subgraphs and sources:
    // native_roots_store re-reads the OS trust store on every call.
    let subgraph_tls_root_store: RootCertStore = configuration
        .tls
        .subgraph
        .all
        .create_certificate_store()
        .transpose()?
        .unwrap_or_else(HttpClientService::native_roots_store);
    let connector_tls_root_store: RootCertStore = configuration
        .tls
        .connector
        .all
        .create_certificate_store()
        .transpose()?
        .unwrap_or_else(HttpClientService::native_roots_store);

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

    let mut subgraph_material = IndexMap::new();
    for (name, _) in schema.subgraphs() {
        if connector_subgraphs.contains(name) {
            continue; // Connector-backed subgraphs get per-source clients below instead
        }
        let material = HttpClientMaterial::for_subgraph(
            name,
            configuration,
            &subgraph_tls_root_store,
            shaping.subgraph_client_config(name),
        )?;
        subgraph_material.insert(name.clone(), material);
    }

    let connector_sources = schema
        .connectors
        .as_ref()
        .map(|c| c.source_config_keys.clone())
        .unwrap_or_default();

    let mut connector_material = IndexMap::new();
    for name in connector_sources.iter() {
        let material = HttpClientMaterial::for_connector(
            name,
            configuration,
            &connector_tls_root_store,
            shaping.connector_client_config(name),
        )?;
        connector_material.insert(name.clone(), material);
    }

    Ok((subgraph_material, connector_material))
}

/// Connects the Redis client for the query-plan cache, when one is configured.
pub(crate) async fn connect_query_plan_redis(
    configuration: &Configuration,
) -> Result<Option<RedisCacheStorage>, BoxError> {
    let cache_config: crate::configuration::Cache =
        configuration.supergraph.query_planning.cache.clone().into();
    match cache_config.redis {
        // Wrap in ServiceBuildError so a refused connection surfaces at startup as
        // "couldn't build Router service: ...".
        Some(redis_config) => connect_redis(redis_config, "query planner")
            .await
            .map_err(|e| crate::error::ServiceBuildError::ServiceError(e).into()),
        None => Ok(None),
    }
}

/// Connects the Redis client for the APQ cache, when APQ is enabled and configured with
/// Redis.
pub(crate) async fn connect_apq_redis(
    configuration: &Configuration,
) -> Result<Option<RedisCacheStorage>, BoxError> {
    if !configuration.apq.enabled {
        return Ok(None);
    }
    match configuration.apq.router.cache.redis.clone() {
        Some(redis_config) => connect_redis(redis_config, "APQ").await,
        None => Ok(None),
    }
}

/// Builds the query-plan cache around a pre-connected Redis client and registers its
/// gauges. Belongs to the assemble phase; the module docs explain why cache construction
/// follows plugin activation.
pub(crate) fn build_query_plan_cache(
    configuration: &Configuration,
    redis: Option<RedisCacheStorage>,
) -> QueryPlanCache {
    Arc::new(DeduplicatingCache::with_capacity(
        configuration
            .supergraph
            .query_planning
            .cache
            .in_memory
            .limit,
        redis,
        "query planner",
    ))
}

/// Builds the APQ expander around a pre-connected Redis client and registers its cache
/// gauges. When APQ is disabled the expander rejects persisted-query hashes instead.
/// Belongs to the assemble phase; the module docs explain why cache construction follows
/// plugin activation.
pub(crate) fn build_apq_expander(
    configuration: &Configuration,
    redis: Option<RedisCacheStorage>,
) -> APQExpander {
    if configuration.apq.enabled {
        APQExpander::with_cache(DeduplicatingCache::with_capacity(
            configuration.apq.router.cache.in_memory.limit,
            redis,
            "APQ",
        ))
    } else {
        APQExpander::disabled()
    }
}

/// Builds HTTP client service factories from parsed client material, keyed as
/// [`HttpClientMaterialMaps`] documents.
pub(crate) fn build_http_services(
    subgraph_material: IndexMap<String, HttpClientMaterial>,
    connector_material: IndexMap<String, HttpClientMaterial>,
    plugins: &Arc<Plugins>,
) -> (
    IndexMap<String, HttpClientServiceFactory>,
    IndexMap<String, HttpClientServiceFactory>,
) {
    let build = |material: IndexMap<String, HttpClientMaterial>| {
        material
            .into_iter()
            .map(|(name, material)| {
                let factory = HttpClientServiceFactory::new(
                    HttpClientService::new(material),
                    plugins.clone(),
                );
                (name, factory)
            })
            .collect()
    };
    (build(subgraph_material), build(connector_material))
}

/// Builds a subgraph service around each entry's HTTP client factory, keyed by subgraph
/// name.
pub(crate) fn create_subgraph_services(
    http_service_factory: &IndexMap<String, HttpClientServiceFactory>,
) -> IndexMap<String, subgraph::BoxCloneService> {
    let mut subgraph_services = IndexMap::default();
    for (name, http_service_factory) in http_service_factory.iter() {
        let svc = SubgraphService::new(name, http_service_factory.create(name));
        subgraph_services.insert(name.clone(), svc.boxed_clone());
    }
    subgraph_services
}

#[allow(clippy::too_many_arguments)]
async fn add_plugin(
    name: String,
    factory: &PluginFactory,
    plugin_config: &Value,
    previous_plugin_config: Option<&Value>,
    schema: Arc<String>,
    schema_id: Arc<String>,
    supergraph_schema: Arc<Valid<apollo_compiler::Schema>>,
    subgraph_schemas: Arc<crate::query_planner::SubgraphSchemas>,
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

/// Instantiates every plugin in the order below, then returns them keyed by name.
///
/// Each Apollo plugin is added through one of three macros:
///
/// - `add_mandatory_apollo_plugin!` instantiates the plugin even with no user config for it.
/// - `add_optional_apollo_plugin!` instantiates the plugin only if configured. The license's
///   allowed features gate it.
/// - `add_oss_apollo_plugin!` instantiates the plugin only if configured, with no license
///   check.
///
/// The macros exist to avoid repeating the several arguments every plugin construction
/// needs; each forwards to [`add_plugin`].
pub(crate) async fn create_plugins(
    configuration: &Configuration,
    schema: &Schema,
    subgraph_schemas: Arc<crate::query_planner::SubgraphSchemas>,
    initial_telemetry_plugin: Option<Box<dyn DynPlugin>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
    previous_config: Option<Arc<Configuration>>,
) -> Result<Plugins, BoxError> {
    let supergraph_schema = Arc::new(schema.supergraph_schema().clone());
    let supergraph_schema_id = schema.schema_id.clone().into_inner();
    let mut apollo_plugins_config = configuration.apollo_plugins.clone().plugins;
    let user_plugins_config = configuration.plugins.clone().plugins.unwrap_or_default();

    // Extract previous plugin configurations for hot reload previous config detection
    let (previous_apollo_plugins_config, previous_user_plugins_config) = match &previous_config {
        Some(config) => {
            // Extract apollo plugin configs from the previous router's stored configuration
            let prev_apollo_configs: HashMap<&str, &Value> = config
                .apollo_plugins
                .plugins
                .iter()
                .map(|(k, v)| (k.as_str(), v))
                .collect();

            // Extract user plugin configs from the previous router's stored configuration
            let prev_user_configs: HashMap<String, &Value> = config
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
    // service*. Depending on the requirements of plugins, it may have to be in this order.
    // Similarly, header propagation being first does not mean that it's exempt from rate
    // limiting, for the same reason. Rate limiting must be after telemetry, though, because
    // telemetry and rate limiting both work at the router service, and requests rejected from
    // the router service must flow through telemetry so we can record errors.
    //
    // Broadly, for telemetry to record errors, we must make sure the telemetry plugin runs
    // before any plugin that can *reject* a request at the router service. Plugins whose
    // router-service hook is an infallible `map_request` (eg `headers`, which only injects
    // `MaskingRulesMap` into context) may appear before telemetry without breaking this
    // invariant — they can't short-circuit a request away from telemetry.
    //
    // Each entry below names the services the plugin hooks. Two plugins whose hooked
    // services don't overlap can be reordered relative to each other; the annotations make
    // that check possible without reading every plugin's source.
    add_mandatory_apollo_plugin!("include_subgraph_errors"); // supergraph, subgraph
    add_mandatory_apollo_plugin!("headers"); // router, subgraph, connector_request
    if apollo_telemetry_plugin_mandatory {
        match initial_telemetry_plugin {
            None => {
                // router, supergraph, execution, subgraph, connector_request, http_client —
                // must come before any plugin below that can reject a request at the router
                // service, so telemetry records the rejection.
                add_mandatory_apollo_plugin!("telemetry");
            }
            Some(plugin) => {
                let _ = plugin_instances.insert("apollo.telemetry".to_string(), plugin);
                apollo_plugins_config.remove("apollo.telemetry");
                apollo_plugin_factories.remove("apollo.telemetry");
            }
        }
    }
    add_mandatory_apollo_plugin!("license_enforcement"); // router
    add_mandatory_apollo_plugin!("health_check"); // router
    add_mandatory_apollo_plugin!("traffic_shaping"); // router, subgraph, connector_request
    add_mandatory_apollo_plugin!("limits"); // router, subgraph, connector_request
    add_mandatory_apollo_plugin!("csrf"); // router
    add_mandatory_apollo_plugin!("fleet_detector"); // router, http_client
    add_mandatory_apollo_plugin!("enhanced_client_awareness"); // supergraph
    add_mandatory_apollo_plugin!("experimental_diagnostics"); // no service hooks

    add_oss_apollo_plugin!("forbid_mutations"); // execution
    add_optional_apollo_plugin!("subscription"); // subgraph
    add_oss_apollo_plugin!("override_subgraph_url"); // subgraph
    add_optional_apollo_plugin!("authorization"); // supergraph, execution
    add_optional_apollo_plugin!("authentication"); // router, subgraph, connector_request
    add_oss_apollo_plugin!("preview_file_uploads"); // router, supergraph, execution, subgraph, http_client
    add_mandatory_apollo_plugin!("progressive_override"); // router, supergraph
    add_optional_apollo_plugin!("demand_control"); // execution, subgraph

    // This relative ordering is documented publicly for native plugins
    // (/graphos/routing/customization/native-plugins):
    add_oss_apollo_plugin!("connectors"); // supergraph, execution
    add_oss_apollo_plugin!("rhai"); // router, supergraph, execution, subgraph
    add_optional_apollo_plugin!("coprocessor"); // router, supergraph, execution, subgraph, connector_request
    add_optional_apollo_plugin!("response_cache"); // supergraph, subgraph
    add_user_plugins!();

    // Because this plugin intercepts subgraph requests
    // and does not forward them to the next service in the chain,
    // it needs to intervene after user plugins for users plugins to run at all.
    add_optional_apollo_plugin!("experimental_mock_subgraphs"); // subgraph

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

pub(crate) fn inject_schema_id(
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
mod tests {
    use serde_json_bytes::json;

    use super::*;
    use crate::services::SupergraphRequest;

    /// Subgraph names in `testdata/supergraph.graphql`, sorted.
    const FIXTURE_SUBGRAPHS: [&str; 4] = ["accounts", "inventory", "products", "reviews"];

    fn test_configuration() -> Arc<Configuration> {
        Arc::new(Configuration::builder().build().unwrap())
    }

    fn test_schema(configuration: &Configuration) -> Arc<Schema> {
        Arc::new(Schema::parse(include_str!("testdata/supergraph.graphql"), configuration).unwrap())
    }

    fn sorted_keys<V>(map: &IndexMap<String, V>) -> Vec<&str> {
        let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// A plugin map holding a real traffic-shaping plugin, which
    /// [`parse_http_client_material`] looks up by name for per-subgraph client config.
    async fn plugins_with_traffic_shaping() -> Plugins {
        let traffic_shaping = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_TRAFFIC_SHAPING)
            .expect("traffic shaping plugin is registered")
            .create_instance_without_schema(&serde_json::json!({}))
            .await
            .expect("traffic shaping plugin builds from an empty config");
        let mut plugins = Plugins::default();
        plugins.insert(APOLLO_TRAFFIC_SHAPING.to_string(), traffic_shaping);
        plugins
    }

    /// A request carrying a persisted-query hash and no query string. An enabled APQ
    /// expander answers it from its cache; a disabled one rejects it. The two error
    /// messages tell the paths apart.
    fn hash_only_apq_request() -> SupergraphRequest {
        SupergraphRequest::fake_builder()
            .extension(
                "persistedQuery",
                json!({
                    "version": 1,
                    "sha256Hash": "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"
                }),
            )
            .build()
            .expect("valid request")
    }

    #[test]
    fn create_query_planner_service_extracts_a_schema_per_subgraph() {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);

        let (_planner, subgraph_schemas) =
            create_query_planner_service(&schema, &configuration).unwrap();

        let mut names: Vec<&str> = subgraph_schemas.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, FIXTURE_SUBGRAPHS);
    }

    #[tokio::test]
    async fn parse_http_client_material_covers_every_subgraph() {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let plugins = plugins_with_traffic_shaping().await;

        let (subgraph_material, connector_material) =
            parse_http_client_material(&plugins, &schema, &configuration).unwrap();

        assert_eq!(sorted_keys(&subgraph_material), FIXTURE_SUBGRAPHS);
        assert!(connector_material.is_empty());
    }

    #[tokio::test]
    async fn build_http_services_builds_a_client_factory_per_subgraph() {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let plugins = plugins_with_traffic_shaping().await;
        let (subgraph_material, connector_material) =
            parse_http_client_material(&plugins, &schema, &configuration).unwrap();

        let (subgraph_factories, connector_factories) =
            build_http_services(subgraph_material, connector_material, &Arc::new(plugins));

        assert_eq!(sorted_keys(&subgraph_factories), FIXTURE_SUBGRAPHS);
        assert!(connector_factories.is_empty());
    }

    #[tokio::test]
    async fn init_telemetry_returns_no_plugin_on_hot_reload() {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let license = Arc::new(LicenseState::default());

        let plugin = init_telemetry(&configuration, &schema, &license, Some(&configuration))
            .await
            .unwrap();

        assert!(plugin.is_none());
    }

    #[tokio::test]
    async fn create_plugins_instantiates_mandatory_plugins() {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let (_planner, subgraph_schemas) =
            create_query_planner_service(&schema, &configuration).unwrap();

        let plugins = create_plugins(
            &configuration,
            &schema,
            subgraph_schemas,
            None,
            None,
            Arc::new(LicenseState::default()),
            None,
        )
        .await
        .unwrap();

        assert!(plugins.contains_key("apollo.include_subgraph_errors"));
        assert!(plugins.contains_key("apollo.traffic_shaping"));
    }

    #[tokio::test]
    async fn build_query_plan_cache_without_redis_uses_the_configured_capacity() {
        let configuration: Configuration = serde_yaml::from_str(
            r#"
            supergraph:
              query_planning:
                cache:
                  in_memory:
                    limit: 42
            "#,
        )
        .unwrap();

        let cache = build_query_plan_cache(&configuration, None);

        assert_eq!(cache.in_memory_cache().lock().await.cap().get(), 42);
    }

    #[tokio::test]
    async fn apq_expander_enabled_reports_an_unknown_hash_as_not_found() {
        let configuration = test_configuration();
        assert!(configuration.apq.enabled);

        let expander = build_apq_expander(&configuration, None);
        let mut response = expander
            .supergraph_request(hash_only_apq_request())
            .await
            .expect_err("a cache miss short-circuits the request");

        let graphql = response.next_response().await.expect("one response");
        assert_eq!(graphql.errors[0].message, "PersistedQueryNotFound");
    }

    #[tokio::test]
    async fn apq_expander_disabled_rejects_persisted_query_requests() {
        let mut configuration = Configuration::default();
        configuration.apq.enabled = false;

        let expander = build_apq_expander(&configuration, None);
        let mut response = expander
            .supergraph_request(hash_only_apq_request())
            .await
            .expect_err("persisted queries are rejected when APQ is disabled");

        let graphql = response.next_response().await.expect("one response");
        assert_eq!(graphql.errors[0].message, "PersistedQueryNotSupported");
    }

    #[tokio::test]
    async fn connect_query_plan_redis_is_none_without_redis_config() {
        let configuration = test_configuration();

        let redis = connect_query_plan_redis(&configuration).await.unwrap();

        assert!(redis.is_none());
    }

    #[tokio::test]
    async fn connect_apq_redis_is_none_without_redis_config() {
        let configuration = test_configuration();

        let redis = connect_apq_redis(&configuration).await.unwrap();

        assert!(redis.is_none());
    }
}
