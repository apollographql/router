//! The assemble phase of [`build_pipeline`](super::build_pipeline): the construction-time
//! tower stack for each pipeline stage, built infallibly from acquired resources.

use std::sync::Arc;

use indexmap::IndexMap;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Configuration;
use crate::batching::BatchQueryPlanAnalysisLayer;
use crate::cache::DeduplicatingCache;
use crate::cache::redis::RedisCacheStorage;
use crate::introspection;
use crate::introspection::IntrospectionService;
use crate::layers::InternalServiceBuilderExt as _;
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
use crate::services::SubgraphServices;
use crate::services::connector::request_service::ConnectorRequestServices;
use crate::services::connector_service::ConnectorServices;
use crate::services::execution;
use crate::services::execution::service::ExecutionService;
use crate::services::fetch_service::FetchService;
use crate::services::http;
use crate::services::http::build_http_client_service;
use crate::services::http::service::HttpClientInputs;
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
/// gauges.
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

/// Builds HTTP client services from parsed client inputs, keyed as
/// [`HttpClientInputsMaps`](super::acquire::HttpClientInputsMaps) documents.
pub(crate) fn build_http_services(
    subgraph_inputs: IndexMap<String, HttpClientInputs>,
    connector_inputs: IndexMap<String, HttpClientInputs>,
    plugins: &Arc<Plugins>,
) -> (
    IndexMap<String, http::BoxCloneService>,
    IndexMap<String, http::BoxCloneService>,
) {
    let subgraph_services = subgraph_inputs
        .into_iter()
        .map(|(name, inputs)| {
            let service = build_http_client_service(&name, inputs, plugins.clone());
            (name, service)
        })
        .collect();
    let connector_services = connector_inputs
        .into_iter()
        .map(|(source, inputs)| {
            // source_config_key() format is "{subgraph_name}.{source_or_synthetic}"; the
            // http_client_service plugin hook receives the subgraph name, not the source key.
            let subgraph_name = source.split('.').next().unwrap_or(&source);
            let service = build_http_client_service(subgraph_name, inputs, plugins.clone());
            (source, service)
        })
        .collect();
    (subgraph_services, connector_services)
}

/// Builds a subgraph service around each entry's HTTP client, keyed by subgraph name.
pub(crate) fn build_subgraph_services(
    http_services: IndexMap<String, http::BoxCloneService>,
) -> IndexMap<String, subgraph::BoxCloneService> {
    http_services
        .into_iter()
        .map(|(name, http_service)| {
            let svc = SubgraphService::new(&name, http_service);
            (name, svc.boxed_clone())
        })
        .collect()
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

pub(crate) struct SupergraphPipeline {
    /// The supergraph service; clone it for each consumer.
    pub(crate) supergraph_service: supergraph::BoxCloneService,
    /// Handle to the in-memory query-plan cache, kept for the next hot reload's warm-up.
    pub(crate) in_memory_query_plan_cache: InMemoryQueryPlanCache,
    /// The caching-wrapped planner, for replaying queries into the plan cache.
    pub(crate) caching_query_planner: query_planner::CacheBoxCloneService,
}

/// Assembles the supergraph pipeline: wraps the query planner in `query_plan_cache`, then
/// builds the execution and supergraph service stacks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_supergraph_pipeline(
    query_planner_service: query_planner::BoxCloneService,
    query_plan_cache: QueryPlanCache,
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    configuration: Arc<Configuration>,
    plugins: Arc<Plugins>,
    subgraph_services: Vec<(String, subgraph::BoxCloneService)>,
    connector_http_services: IndexMap<String, http::BoxCloneService>,
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
        connector_http_services,
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
        supergraph_service,
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
    connector_http_services: IndexMap<String, http::BoxCloneService>,
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
        Arc::new(SubgraphServices::new(
            subgraph_services,
            plugins.clone(),
            configuration.notify.clone(),
            subscription_plugin_conf.clone().map(Arc::new),
            configuration.apq.subgraph.clone(),
        )),
        subscription_plugin_conf.clone(),
        Arc::new(ConnectorServices::new(
            schema.clone(),
            subgraph_schemas.clone(),
            subscription_plugin_conf.clone(),
            schema
                .connectors
                .as_ref()
                .map(|c| c.by_service_name.clone())
                .unwrap_or_default(),
            Arc::new(ConnectorRequestServices::new(
                connector_http_services,
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

/// Assembles the [`SupergraphService`] stack, outermost first: the content-negotiation
/// and compute-job metrics layers, each plugin's `supergraph_service` hook, and the
/// mutation-restriction and operation-limit layers.
fn build_supergraph_service(
    query_planner_service: query_planner::CacheBoxCloneService,
    execution_service: execution::BoxCloneService,
    introspection_service: IntrospectionService,
    schema: Arc<Schema>,
    configuration: &Configuration,
    plugins: Arc<Plugins>,
) -> supergraph::BoxCloneService {
    let supergraph_service = SupergraphService::builder()
        .query_planner_service(query_planner_service)
        .execution_service(execution_service)
        .introspection_service(introspection_service)
        .schema(schema)
        .strict_variable_validation(configuration.supergraph.strict_variable_validation)
        .build();

    ServiceBuilder::new()
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
