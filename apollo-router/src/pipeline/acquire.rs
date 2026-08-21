//! The acquire phase of [`build_pipeline`](super::build_pipeline): every resource whose
//! creation can fail.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use rustls::RootCertStore;
use tower::BoxError;
use tower::ServiceExt;
use tracing::Instrument;

use super::plugins::create_plugins;
use super::plugins::inject_schema_id;
use crate::cache::redis::RedisCacheStorage;
use crate::cache::storage::connect_redis;
use crate::configuration::Configuration;
use crate::plugin::DynPlugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::query_planner::QueryPlannerService;
use crate::query_planner::SubgraphSchemas;
use crate::services::Plugins;
use crate::services::http::HttpClientService;
use crate::services::http::service::HttpClientMaterial;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::query_planner;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Everything the acquire phase produces. Each field except `subgraph_schemas` (an
/// infallible byproduct of planner creation) is a resource whose creation can fail; the
/// assemble phase consumes them infallibly.
pub(super) struct Acquired {
    pub(super) query_planner_service: query_planner::BoxCloneService,
    pub(super) subgraph_schemas: Arc<SubgraphSchemas>,
    pub(super) plugins: Arc<Plugins>,
    pub(super) subgraph_client_material: IndexMap<String, HttpClientMaterial>,
    pub(super) connector_client_material: IndexMap<String, HttpClientMaterial>,
    pub(super) query_plan_redis: Option<RedisCacheStorage>,
    pub(super) apq_redis: Option<RedisCacheStorage>,
    pub(super) persisted_queries: Arc<PersistedQueryExpander>,
}

pub(super) async fn acquire(
    configuration: &Arc<Configuration>,
    schema: &Arc<Schema>,
    previous_config: Option<Arc<Configuration>>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
) -> Result<Acquired, BoxError> {
    let bootstrap_telemetry_plugin =
        maybe_bootstrap_telemetry(configuration, schema, &license, previous_config.as_deref())
            .await?;

    let (query_planner_service, subgraph_schemas) =
        create_query_planner_service(schema, configuration)?;

    let plugins: Arc<Plugins> = Arc::new(
        create_plugins(
            configuration,
            schema,
            subgraph_schemas.clone(),
            bootstrap_telemetry_plugin,
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
pub(super) async fn maybe_bootstrap_telemetry(
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

            // No previous config: this branch only runs on first boot (`previous_config`
            // is `None` per the guard above).
            let telemetry_init = PluginInit::builder()
                .config(plugin_config.clone())
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
/// Fails when the schema uses unsupported federation versions or features, when its
/// authorization directives do not validate, or when planner initialization fails for
/// another reason.
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

/// Parses TLS and DNS client material for every non-connector subgraph and every
/// connector source.
///
/// This is the fallible half of HTTP client construction;
/// [`build_http_services`](super::build_http_services) turns the material into services.
pub(crate) fn parse_http_client_material(
    plugins: &Plugins,
    schema: &Schema,
    configuration: &Configuration,
) -> Result<HttpClientMaterialMaps, BoxError> {
    // Build the subgraph and connector root stores once each and share them across
    // their clients:
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
