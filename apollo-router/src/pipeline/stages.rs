//! The assemble phase of [`build_pipeline`](super::build_pipeline): the construction-time
//! tower stack for each pipeline stage, built infallibly from acquired resources.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::Configuration;
use crate::apollo_studio_interop::extended_references_layer::ExtendedReferencesLayer;
use crate::batching::BatchQueryPlanAnalysisLayer;
use crate::batching::SplitBatchRequestLayer;
use crate::cache::DeduplicatingCache;
use crate::cache::redis::RedisCacheStorage;
use crate::introspection;
use crate::introspection::IntrospectionService;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::InternalServiceBuilderExt as _;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::extract_authorization_checks_layer::ExtractAuthorizationChecksLayer;
use crate::plugins::connectors::tracing::connect_spec_version_instrument;
use crate::plugins::limits::operation_limits_layer::EnforceOperationLimitsLayer;
use crate::plugins::subscription::APOLLO_SUBSCRIPTION_PLUGIN;
use crate::plugins::subscription::Subscription;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::SubscriptionExecutionLayer;
use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config::ApolloMetricsReferenceMode;
use crate::plugins::telemetry::config::Conf as TelemetryConfig;
use crate::query_planner::CachingQueryPlanner;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::QueryPlanCache;
use crate::query_planner::SubgraphSchemas;
use crate::query_planner::warmup;
use crate::services::Plugins;
use crate::services::SubgraphService;
use crate::services::SubgraphServices;
use crate::services::connector::request_service::ConnectorRequestService;
use crate::services::connector::request_service::ConnectorRequestServices;
use crate::services::connector_service::ConnectorService;
use crate::services::connector_service::ConnectorServices;
use crate::services::execution;
use crate::services::execution::service::ExecutionService;
use crate::services::fetch_service::FetchService;
use crate::services::http;
use crate::services::http::build_http_client_service;
use crate::services::http::service::HttpClientInputs;
use crate::services::layers::allow_only_http_post_mutations::AllowOnlyHttpPostMutationsLayer;
use crate::services::layers::apq::APQExpander;
use crate::services::layers::apq::subgraph::SubgraphApqLayer;
use crate::services::layers::content_negotiation;
use crate::services::layers::persisted_queries::EnforceSafelistLayer;
use crate::services::layers::persisted_queries::ExpandIdsLayer;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::layers::static_page::StaticPageLayer;
use crate::services::query_parsing;
use crate::services::query_parsing::cache::QueryParsingCacheLayer;
use crate::services::query_parsing::recursive_selections_limit::LimitRecursiveSelectionLayer;
use crate::services::query_parsing::service::QueryParsingService;
use crate::services::query_planner;
use crate::services::router;
use crate::services::router::parse_query::ParseQueryLayer;
use crate::services::router::service::DisplayRouterRequestLayer;
use crate::services::router::service::RouterToSupergraphRequestLayer;
use crate::services::router::tower_compat::APQCachingLayer;
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

/// Assembles the full service stack for each subgraph, keyed by subgraph name: a
/// [`SubgraphService`] around the subgraph's HTTP client, wrapped by
/// [`wrap_subgraph_services`].
pub(crate) fn build_subgraph_services(
    http_services: IndexMap<String, http::BoxCloneService>,
    plugins: &Arc<Plugins>,
    configuration: &Configuration,
) -> SubgraphServices {
    let subgraph_services = http_services
        .into_iter()
        .map(|(name, http_service)| {
            let svc = SubgraphService::new(&name, http_service);
            (name, svc.boxed_clone())
        })
        .collect();
    wrap_subgraph_services(subgraph_services, plugins, configuration)
}

/// Wraps each subgraph service in its per-subgraph stack, outermost first: a buffer,
/// each plugin's `subgraph_service` hook, and the subscription, APQ, and
/// content-negotiation layers.
pub(crate) fn wrap_subgraph_services(
    subgraph_services: Vec<(String, subgraph::BoxCloneService)>,
    plugins: &Arc<Plugins>,
    configuration: &Configuration,
) -> SubgraphServices {
    use crate::layers::ServiceBuilderExt as _;

    let subscription_config = subscription_plugin_config(plugins).map(Arc::new);

    let mut map = HashMap::with_capacity(subgraph_services.len());
    for (name, service) in subgraph_services.into_iter() {
        // The subscription and APQ layers sit *after* all user plugins but *before* the
        // subgraph service proper.
        let apq_enabled = configuration.apq.subgraph.get(&name).enabled;

        // One buffer per named subgraph provides per-subgraph backpressure and is
        // required for correct LoadShed / RateLimit behaviour from traffic-shaping
        // plugins (see ServiceBuilderExt::buffered).
        let service = ServiceBuilder::new()
            .buffered()
            .rust_plugins(plugins.clone(), |plugin, service| {
                plugin.subgraph_service(&name, service)
            })
            .layer(SubscriptionSubgraphLayer::new(
                configuration.notify.clone(),
                subscription_config.clone(),
                Arc::from(name.clone()),
            ))
            .layer(SubgraphApqLayer::new(apq_enabled))
            .layer(content_negotiation::SubgraphContentNegotiationLayer::default())
            .service(service);

        map.insert(name, service);
    }

    SubgraphServices {
        services: Arc::new(map),
    }
}

/// Assembles the request service stack for each connector source, keyed by
/// `source_config_key()`, outermost first: a per-source buffer, each plugin's
/// `connector_request_service` hook, and the [`ConnectorRequestService`] around the
/// source's HTTP client.
fn build_connector_request_services(
    connector_http_services: IndexMap<String, http::BoxCloneService>,
    plugins: &Arc<Plugins>,
) -> ConnectorRequestServices {
    let mut map = HashMap::with_capacity(connector_http_services.len());
    for (source, http_client) in connector_http_services.into_iter() {
        // One buffer per connector source provides per-source backpressure and is
        // required for correct LoadShed / RateLimit behaviour from traffic-shaping
        // plugins (mirrors the per-subgraph buffer in [`wrap_subgraph_services`]).
        let service = UnconstrainedBuffer::new(
            plugins.iter().rev().fold(
                ConnectorRequestService { http_client }.boxed_clone(),
                |acc, (_, e)| e.connector_request_service(acc, source.clone()),
            ),
            DEFAULT_BUFFER_SIZE,
        );
        map.insert(source, service);
    }

    ConnectorRequestServices {
        services: Arc::new(map),
    }
}

/// Assembles the [`ConnectorService`] stack for each of the schema's connectors, keyed
/// by the connector's service name: a per-connector buffer around the
/// [`ConnectorService`] holding its source's request service.
fn build_connector_services(
    schema: Arc<Schema>,
    subgraph_schemas: Arc<SubgraphSchemas>,
    subscription_config: Option<SubscriptionConfig>,
    connector_request_services: ConnectorRequestServices,
) -> ConnectorServices {
    let connectors_by_service_name = schema
        .connectors
        .as_ref()
        .map(|c| c.by_service_name.clone())
        .unwrap_or_default();

    let mut services = HashMap::with_capacity(connectors_by_service_name.len());
    for (service_name, connector) in connectors_by_service_name.iter() {
        let connector_request_service =
            connector_request_services.get(connector.source_config_key());

        let service = ConnectorService {
            _schema: schema.clone(),
            _subgraph_schemas: subgraph_schemas.clone(),
            _subscription_config: subscription_config.clone(),
            connector: connector.clone(),
            connector_request_service,
        };
        services.insert(
            service_name.to_string(),
            UnconstrainedBuffer::new(service.boxed_clone(), DEFAULT_BUFFER_SIZE),
        );
    }

    ConnectorServices {
        connectors_by_service_name,
        _connect_spec_version_instrument: connect_spec_version_instrument(
            schema.connectors.as_ref(),
        ),
        services: Arc::new(services),
    }
}

/// The subscription plugin's configuration, when the plugin is installed.
fn subscription_plugin_config(plugins: &Plugins) -> Option<SubscriptionConfig> {
    plugins
        .iter()
        .find(|i| i.0.as_str() == APOLLO_SUBSCRIPTION_PLUGIN)
        .and_then(|plugin| (*plugin.1).as_any().downcast_ref::<Subscription>())
        .map(|p| p.config.clone())
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
    subgraph_services: SubgraphServices,
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
    subgraph_services: SubgraphServices,
    connector_http_services: IndexMap<String, http::BoxCloneService>,
    configuration: Arc<Configuration>,
) -> execution::BoxCloneService {
    let subscription_plugin_conf = subscription_plugin_config(&plugins);

    let connector_services = build_connector_services(
        schema.clone(),
        subgraph_schemas.clone(),
        subscription_plugin_conf.clone(),
        build_connector_request_services(connector_http_services, &plugins),
    );

    let fetch_service = FetchService::new(
        schema.clone(),
        subgraph_schemas.clone(),
        Arc::new(subgraph_services),
        subscription_plugin_conf.clone(),
        Arc::new(connector_services),
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

/// Assembles the router service stack, outermost first: the static-page layer, each
/// plugin's `router_service` hook, content negotiation, request logging, batch
/// splitting, the HTTP→GraphQL translation, persisted-query and APQ expansion, query
/// parsing, authorization and extended-reference extraction, and safelist enforcement,
/// ending in the supergraph service.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_router_service(
    supergraph_service: supergraph::BoxCloneService,
    apq_expander: APQExpander,
    persisted_queries: Arc<PersistedQueryExpander>,
    query_parsing_service: query_parsing::BoxCloneService,
    schema: Arc<Schema>,
    configuration: &Configuration,
    plugins: Arc<Plugins>,
) -> router::BoxCloneService {
    let enable_authorization_directives =
        AuthorizationPlugin::enable_directives(configuration, &schema).unwrap_or(false);
    let extended_references = matches!(
        TelemetryConfig::metrics_reference_mode(configuration),
        ApolloMetricsReferenceMode::Extended
    );

    ServiceBuilder::new()
        .layer(StaticPageLayer::new(configuration))
        .rust_plugins(plugins, |plugin, service| plugin.router_service(service))
        .layer(content_negotiation::RouterContentNegotiationLayer::default())
        .layer(DisplayRouterRequestLayer)
        .layer(SplitBatchRequestLayer::new(configuration.batching.clone()))
        .layer(RouterToSupergraphRequestLayer)
        .layer(ExpandIdsLayer::new(persisted_queries.clone()))
        .layer(APQCachingLayer::new(Arc::new(apq_expander)))
        .layer(ParseQueryLayer::new(
            query_parsing_service,
            configuration.supergraph.redact_query_validation_errors,
        ))
        .option_layer(
            enable_authorization_directives
                .then(|| ExtractAuthorizationChecksLayer::new(schema.clone())),
        )
        .option_layer(extended_references.then(|| ExtendedReferencesLayer::new(schema)))
        .layer(EnforceSafelistLayer::new(persisted_queries))
        .service(supergraph_service)
        .boxed_clone()
}

/// Assembles the query-plan warm-up service: parses each warm-up query, feeds it to the
/// caching query planner to populate the plan cache, and discards the responses.
pub(crate) fn build_warmup_service(
    query_parsing_service: query_parsing::BoxCloneService,
    caching_query_planner: query_planner::CacheBoxCloneService,
) -> warmup::BoxCloneService {
    ServiceBuilder::new()
        .layer(warmup::WarmupParseQueryLayer::new(query_parsing_service))
        .map_response(drop) // Ignore response
        .service(caching_query_planner)
        .boxed_clone()
}
