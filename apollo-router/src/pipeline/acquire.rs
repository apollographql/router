//! The acquire phase of [`build_pipeline`](super::build_pipeline): every resource whose
//! creation can fail.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use rustls::RootCertStore;
use tower::BoxError;
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
use apollo_federation::query_plan::query_planner::QueryPlanner;

use crate::query_planner::QueryPlannerService;
use crate::query_planner::SubgraphSchemas;
use crate::services::Plugins;
use crate::services::http::HttpClientService;
use crate::services::http::service::HttpClientInputs;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::subgraph::http::create_certificate_store;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

/// Everything the acquire phase produces; the assemble phase consumes it infallibly.
pub(super) struct Acquired {
    pub(super) query_planner: Arc<QueryPlanner>,
    pub(super) subgraph_schemas: Arc<SubgraphSchemas>,
    pub(super) plugins: Arc<Plugins>,
    pub(super) http_client_inputs: HttpClientInputsMaps,
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
    bootstrap_telemetry_plugin: Option<Box<dyn DynPlugin>>,
) -> Result<Acquired, BoxError> {
    let (query_planner, subgraph_schemas) = create_query_planner(schema, configuration)?;

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

    let http_client_inputs = tracing::info_span!("http_client_inputs")
        .in_scope(|| parse_http_client_inputs(&plugins, schema, configuration))?;

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
        query_planner,
        subgraph_schemas,
        plugins,
        http_client_inputs,
        query_plan_redis,
        apq_redis,
        persisted_queries,
    })
}

/// Creates and activates the telemetry plugin before the rest of pipeline construction.
///
/// Tracing has to be live before the other plugins and services are built, or none of
/// that construction gets traced.
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

/// Creates the federation query planner and the subgraph schemas it extracts.
///
/// # Errors
/// Fails when the schema uses unsupported federation versions or features, or when
/// planner initialization fails for another reason.
pub(crate) fn create_query_planner(
    schema: &Arc<Schema>,
    configuration: &Arc<Configuration>,
) -> Result<(Arc<QueryPlanner>, Arc<SubgraphSchemas>), BoxError> {
    tracing::info_span!("query_planner_creation").in_scope(|| {
        let planner = QueryPlannerService::create_planner(schema, configuration)?;
        let subgraph_schemas = crate::query_planner::build_subgraph_schemas(&planner);

        Ok((planner, subgraph_schemas))
    })
}

/// Parsed HTTP client inputs for every subgraph and connector source.
pub(crate) struct HttpClientInputsMaps {
    /// Client inputs for regular subgraphs, keyed by subgraph name.
    pub(crate) subgraphs: IndexMap<String, HttpClientInputs>,
    /// Client inputs for connector sources, keyed by `source_config_key()`
    /// (`{subgraph_name}.{source_or_synthetic}`).
    pub(crate) connectors: IndexMap<String, HttpClientInputs>,
}

/// Parses TLS and DNS client inputs for every non-connector subgraph and every
/// connector source.
pub(crate) fn parse_http_client_inputs(
    plugins: &Plugins,
    schema: &Schema,
    configuration: &Configuration,
) -> Result<HttpClientInputsMaps, BoxError> {
    // Build the subgraph and connector root stores once each and share them across
    // their clients:
    // native_roots_store re-reads the OS trust store on every call.
    let subgraph_tls_root_store: RootCertStore =
        create_certificate_store(&configuration.tls.subgraph.all)
            .transpose()?
            .unwrap_or_else(HttpClientService::native_roots_store);
    let connector_tls_root_store: RootCertStore =
        create_certificate_store(&configuration.tls.connector.all)
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

    let mut subgraph_inputs = IndexMap::new();
    for (name, _) in schema.subgraphs() {
        if connector_subgraphs.contains(name) {
            continue; // Connector-backed subgraphs get per-source clients below instead
        }
        let inputs = HttpClientInputs::for_subgraph(
            name,
            configuration,
            &subgraph_tls_root_store,
            shaping.subgraph_client_config(name),
        )?;
        subgraph_inputs.insert(name.clone(), inputs);
    }

    let connector_sources = schema
        .connectors
        .as_ref()
        .map(|c| c.source_config_keys.clone())
        .unwrap_or_default();

    let mut connector_inputs = IndexMap::new();
    for name in connector_sources.iter() {
        let inputs = HttpClientInputs::for_connector(
            name,
            configuration,
            &connector_tls_root_store,
            shaping.connector_client_config(name),
        )?;
        connector_inputs.insert(name.clone(), inputs);
    }

    Ok(HttpClientInputsMaps {
        subgraphs: subgraph_inputs,
        connectors: connector_inputs,
    })
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
