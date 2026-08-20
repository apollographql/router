//! The assemble phase of [`build_pipeline`](super::build_pipeline): the construction-time
//! tower stack for each pipeline stage, built infallibly from acquired resources.

use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Configuration;
use crate::batching::BatchQueryPlanAnalysisLayer;
use crate::cache::DeduplicatingCache;
use crate::cache::redis::RedisCacheStorage;
use crate::introspection;
use crate::introspection::IntrospectionService;
use crate::layers::InternalServiceBuilderExt as _;
use crate::layers::ServiceBuilderExt as _;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::plugins::limits::operation_limits_layer::EnforceOperationLimitsLayer;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::SubscriptionExecutionLayer;
use crate::plugins::telemetry::Telemetry;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::QueryPlanCache;
use crate::query_planner::SubgraphSchemas;
use crate::services::Plugins;
use crate::services::SubgraphService;
use crate::services::SubgraphServiceFactory;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::connector::request_service::ConnectorRequestServiceFactory;
use crate::services::connector_service::ConnectorServiceFactory;
use crate::services::execution;
use crate::services::execution::service::ExecutionService;
use crate::services::fetch_service::FetchService;
use crate::services::http::HttpClientService;
use crate::services::http::HttpClientServiceFactory;
use crate::services::http::service::HttpClientMaterial;
use crate::services::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use crate::services::layers::apq::APQExpander;
use crate::services::layers::content_negotiation;
use crate::services::query_parsing;
use crate::services::query_parsing::cache::QueryParsingCacheLayer;
use crate::services::query_parsing::recursive_selections_limit::LimitRecursiveSelectionLayer;
use crate::services::query_parsing::service::QueryParsingService;
use crate::services::query_planner;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::services::supergraph::service::SupergraphService;
use crate::spec::Schema;

/// Builds the query-plan cache around a pre-connected Redis client and registers its
/// gauges. Belongs to the assemble phase; the [`crate::pipeline`] docs explain why cache
/// construction follows plugin activation.
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
/// Belongs to the assemble phase; the [`crate::pipeline`] docs explain why cache
/// construction follows plugin activation.
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
/// [`HttpClientMaterialMaps`](super::acquire::HttpClientMaterialMaps) documents.
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
pub(crate) fn build_subgraph_services(
    http_service_factory: &IndexMap<String, HttpClientServiceFactory>,
) -> IndexMap<String, subgraph::BoxCloneService> {
    let mut subgraph_services = IndexMap::default();
    for (name, http_service_factory) in http_service_factory.iter() {
        let svc = SubgraphService::new(name, http_service_factory.create(name));
        subgraph_services.insert(name.clone(), svc.boxed_clone());
    }
    subgraph_services
}

/// Build a query parsing service with in-memory caching.
///
/// The cache size is the same as the query plan cache size.
pub(crate) fn build_query_parsing_service(
    schema: Arc<Schema>,
    configuration: Arc<Configuration>,
) -> query_parsing::BoxCloneService {
    let cache_limit = configuration
        .supergraph
        .query_planning
        .cache
        .in_memory
        .limit;
    let max_recursive_selections = configuration.limits.router.max_recursive_selections;
    let warn_only = configuration.limits.router.warn_only;

    ServiceBuilder::new()
        .layer(QueryParsingCacheLayer::new(cache_limit))
        .layer(LimitRecursiveSelectionLayer::new(
            max_recursive_selections,
            warn_only,
        ))
        .service(QueryParsingService::new(schema, configuration))
        .boxed_clone()
}

/// What [`build_supergraph_pipeline`] assembles.
pub(crate) struct SupergraphPipeline {
    /// The buffered supergraph service; clone it for each consumer.
    pub(crate) supergraph_service: supergraph::BoxCloneService,
    /// Handle to the in-memory query-plan cache, kept for the next hot reload's warm-up.
    pub(crate) in_memory_query_plan_cache: InMemoryQueryPlanCache,
    /// The caching-wrapped planner, consumed by warm-up.
    pub(crate) caching_query_planner: query_planner::CacheBoxCloneService,
}

/// Assembles the supergraph pipeline: wraps the query planner in `query_plan_cache`, then
/// builds the execution and supergraph service stacks.
///
/// Part of the assemble phase; call it after plugin activation, with a cache built by
/// [`build_query_plan_cache`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_supergraph_pipeline(
    query_planner_service: query_planner::BoxCloneService,
    query_plan_cache: QueryPlanCache,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    configuration: Arc<Configuration>,
    plugins: Arc<Plugins>,
    subgraph_services: Vec<(String, subgraph::BoxCloneService)>,
    connector_http_service_factory: IndexMap<String, HttpClientServiceFactory>,
) -> SupergraphPipeline {
    let query_planner_service = CachingQueryPlanner::new(
        query_planner_service,
        schema.clone(),
        subgraph_schemas.clone(),
        &configuration,
        query_plan_cache.clone(),
    )
    .boxed_clone();

    let introspection_service = introspection::introspection_service(&configuration);

    let execution_service = build_execution_service(
        schema.clone(),
        subgraph_schemas,
        plugins.clone(),
        subgraph_services,
        connector_http_service_factory,
        configuration.clone(),
    );

    let supergraph_service = build_supergraph_service(
        query_planner_service.clone(),
        execution_service,
        introspection_service,
        schema,
        &configuration,
        plugins,
    );

    SupergraphPipeline {
        supergraph_service: supergraph_service.boxed_clone(),
        in_memory_query_plan_cache: query_plan_cache.in_memory_cache(),
        caching_query_planner: query_planner_service,
    }
}

/// Assembles the execution service stack: the batching and subscription layers, each plugin's
/// `execution_service` hook, and the [`ExecutionService`] with its [`FetchService`] that
/// dispatches to subgraphs and connectors.
fn build_execution_service(
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    plugins: Arc<Plugins>,
    subgraph_services: Vec<(String, subgraph::BoxCloneService)>,
    connector_http_service_factory: IndexMap<String, HttpClientServiceFactory>,
    configuration: Arc<Configuration>,
) -> execution::BoxCloneService {
    let subscription_plugin_conf = plugins
        .iter()
        .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
        .map(|p| p.config.clone());

    let fetch_service = FetchService::new(
        schema.clone(),
        subgraph_schemas.clone(),
        Arc::new(SubgraphServiceFactory::new(
            subgraph_services,
            plugins.clone(),
            configuration.notify.clone(),
            subscription_plugin_conf.clone().map(Arc::new),
            configuration.apq.subgraph.clone(),
        )),
        subscription_plugin_conf.clone(),
        Arc::new(ConnectorServiceFactory::new(
            schema.clone(),
            subgraph_schemas.clone(),
            subscription_plugin_conf.clone(),
            schema
                .connectors
                .as_ref()
                .map(|c| c.by_service_name.clone())
                .unwrap_or_default(),
            Arc::new(ConnectorRequestServiceFactory::new(
                Arc::new(connector_http_service_factory),
                plugins.clone(),
            )),
        )),
        Arc::new(configuration.experimental_hoist_orphan_errors.clone()),
    );

    let apollo_telemetry_conf = plugins
        .iter()
        .find(|i| i.0.as_str() == "apollo.telemetry")
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Telemetry>())
        .map(|t| t.config.apollo.clone());

    ServiceBuilder::new()
        .layer(BatchQueryPlanAnalysisLayer::new())
        .layer(SubscriptionExecutionLayer::new(
            configuration.notify.clone(),
        ))
        .rust_plugins(plugins.clone(), |plugin, service| {
            plugin.execution_service(service)
        })
        .service(
            ExecutionService {
                schema,
                fetch_service,
                subscription_config: subscription_plugin_conf,
                subgraph_schemas,
                apollo_telemetry_config: apollo_telemetry_conf,
                configuration,
            }
            .boxed_clone(),
        )
        .boxed_clone()
}

/// Assembles the [`SupergraphService`] stack, outermost first: the buffer, the
/// content-negotiation and compute-job metrics layers, each plugin's `supergraph_service`
/// hook, and the mutation-restriction and operation-limit layers.
fn build_supergraph_service(
    query_planner_service: query_planner::CacheBoxCloneService,
    execution_service: execution::BoxCloneService,
    introspection_service: IntrospectionService,
    schema: Arc<Schema>,
    configuration: &Configuration,
    plugins: Arc<Plugins>,
) -> UnconstrainedBuffer<SupergraphRequest, BoxFuture<'static, Result<SupergraphResponse, BoxError>>>
{
    let supergraph_service = SupergraphService::builder()
        .query_planner_service(query_planner_service)
        .execution_service(execution_service)
        .introspection_service(introspection_service)
        .schema(schema)
        .strict_variable_validation(configuration.supergraph.strict_variable_validation)
        .build();

    // The outer buffer provides backpressure for the full supergraph pipeline and is
    // required for correct LoadShed / ConcurrencyLimit / RateLimit behaviour introduced
    // by traffic-shaping and other plugins (see ServiceBuilderExt::buffered).
    ServiceBuilder::new()
        .buffered()
        .layer(content_negotiation::SupergraphContentNegotiationLayer::default())
        .layer(crate::compute_job::ComputeJobMetricsLayer::new())
        .rust_plugins(plugins, |plugin, service| {
            plugin.supergraph_service(service)
        })
        .layer(AllowOnlyHttpPostMutationsLayer::default())
        .layer(EnforceOperationLimitsLayer::new(
            &configuration.limits.router,
        ))
        .service(supergraph_service)
}
