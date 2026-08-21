//! Construction of the router's serving pipeline from configuration, schema, and license.
//!
//! [`build_pipeline`] runs three phases, each under its own tracing span:
//!
//! - **Acquire** (the [`acquire`](mod@self::acquire) submodule) gathers every resource whose
//!   creation can fail: the telemetry plugin, the federation query planner, the other
//!   plugins, TLS/DNS client inputs, Redis clients, and the persisted-query manifest.
//! - **Activate** runs every plugin's `activate()` hook. The telemetry hook swaps in global
//!   tracer and meter providers that cannot be rolled back. Nothing after this phase starts
//!   may fail.
//! - **Assemble** (the [`stages`] submodule) builds the caches and service stacks from the
//!   acquired resources, using infallible functions. Each cache registers its gauges in its
//!   constructor; constructing caches after the meter-provider swap binds those gauges to
//!   the provider that serves this pipeline.

use std::sync::Arc;

#[cfg(test)]
use futures::future::BoxFuture;
use multimap::MultiMap;
use tower::BoxError;
#[cfg(test)]
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;

use self::acquire::Acquired;
use self::acquire::acquire;
pub(crate) use self::acquire::connect_apq_redis;
pub(crate) use self::acquire::connect_query_plan_redis;
pub(crate) use self::acquire::create_query_planner_service;
pub(crate) use self::acquire::parse_http_client_inputs;
pub(crate) use self::plugins::create_plugins;
pub(crate) use self::stages::SupergraphPipeline;
pub(crate) use self::stages::build_apq_expander;
pub(crate) use self::stages::build_http_services;
pub(crate) use self::stages::build_query_parsing_service;
pub(crate) use self::stages::build_query_plan_cache;
pub(crate) use self::stages::build_subgraph_services;
pub(crate) use self::stages::build_supergraph_pipeline;
use crate::Endpoint;
use crate::ListenAddr;
use crate::configuration::Configuration;
use crate::layers::InternalServiceBuilderExt as _;
use crate::plugin::DynPlugin;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::warmup;
use crate::router_factory::RouterFactory;
use crate::services::Plugins;
use crate::services::layers::apq::APQExpander;
use crate::services::layers::content_negotiation;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::layers::static_page::StaticPageLayer;
use crate::services::query_parsing;
use crate::services::query_planner;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineHandle;
use crate::services::router::service::RouterService;
use crate::services::supergraph;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

mod acquire;
mod plugins;
mod stages;
#[cfg(test)]
mod tests;

/// Builds a serving pipeline from configuration, schema, and license.
///
/// Runs the acquire, activate, and assemble phases the module docs describe. On a hot
/// reload, pass the previous pipeline's configuration and in-memory query-plan cache.
/// `build_pipeline` then skips the early telemetry activation, and warm-up replays a
/// configurable sample of the previously cached queries against the new planner.
pub(crate) async fn build_pipeline(
    configuration: Arc<Configuration>,
    schema: Arc<Schema>,
    previous_config: Option<Arc<Configuration>>,
    previous_cache: Option<InMemoryQueryPlanCache>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
) -> Result<Pipeline, BoxError> {
    let acquired = acquire(
        &configuration,
        &schema,
        previous_config,
        extra_plugins,
        license,
    )
    .instrument(tracing::info_span!("acquire"))
    .await?;

    activate(&acquired);

    let pipeline =
        tracing::info_span!("assemble").in_scope(|| assemble(acquired, configuration, schema));

    pipeline
        .warm_up(previous_cache)
        .instrument(tracing::info_span!("warmup"))
        .await;

    Ok(pipeline)
}

/// The point of no return: activating the telemetry plugin swaps in global tracer and
/// meter providers that cannot be rolled back. From here on the pipeline must go live,
/// so everything after this call is infallible.
fn activate(acquired: &Acquired) {
    tracing::info_span!("activate").in_scope(|| {
        for (_, plugin) in acquired.plugins.iter() {
            plugin.activate();
        }
    })
}

/// Assembles the pipeline from the acquired resources.
///
/// Call after [`activate`]: the caches built here register their gauges against the
/// meter provider that activation installed.
fn assemble(
    acquired: Acquired,
    configuration: Arc<Configuration>,
    schema: Arc<Schema>,
) -> Pipeline {
    let Acquired {
        query_planner_service,
        subgraph_schemas,
        plugins,
        subgraph_client_inputs,
        connector_client_inputs,
        query_plan_redis,
        apq_redis,
        persisted_queries,
    } = acquired;

    let query_plan_cache = build_query_plan_cache(&configuration, query_plan_redis);
    let apq_expander = build_apq_expander(&configuration, apq_redis);
    let query_parsing_service = build_query_parsing_service(schema.clone(), configuration.clone());

    let SupergraphPipeline {
        supergraph_service,
        in_memory_query_plan_cache,
        caching_query_planner,
    } = tracing::info_span!("supergraph_creation").in_scope(|| {
        let (http_service_factory, connector_http_service_factory) =
            build_http_services(subgraph_client_inputs, connector_client_inputs, &plugins);
        let subgraph_services = build_subgraph_services(&http_service_factory);
        build_supergraph_pipeline(
            query_planner_service,
            query_plan_cache,
            schema.clone(),
            subgraph_schemas,
            configuration.clone(),
            plugins.clone(),
            subgraph_services.into_iter().collect(),
            connector_http_service_factory,
        )
    });

    Pipeline::new(
        persisted_queries,
        apq_expander,
        supergraph_service,
        caching_query_planner,
        schema,
        plugins,
        in_memory_query_plan_cache,
        query_parsing_service,
        configuration,
    )
}

/// Builds an activated supergraph service for [`TestHarness`](crate::TestHarness),
/// without the telemetry early-activation, APQ, persisted queries, warm-up, and router
/// service that [`build_pipeline`] adds around it.
///
/// Composes the same acquire → activate → assemble sequence as [`build_pipeline`],
/// restricted to what the supergraph service needs:
///
/// - acquire the query planner, the plugins, HTTP client inputs, and the query-plan
///   Redis client
/// - activate every plugin
/// - assemble the HTTP and subgraph services, the query-plan cache, and the supergraph
///   pipeline
///
/// A change to that sequence in [`build_pipeline`] applies here too.
pub(crate) async fn build_supergraph_only(
    configuration: Arc<Configuration>,
    schema: Arc<Schema>,
    extra_plugins: Vec<(String, Box<dyn DynPlugin>)>,
    license: Arc<LicenseState>,
) -> Result<(Arc<Plugins>, SupergraphPipeline), BoxError> {
    let (query_planner_service, subgraph_schemas) =
        create_query_planner_service(&schema, &configuration)?;
    let plugins: Arc<Plugins> = Arc::new(
        create_plugins(
            &configuration,
            &schema,
            subgraph_schemas.clone(),
            None,
            Some(extra_plugins),
            license,
            None,
        )
        .instrument(tracing::info_span!("plugins"))
        .await?
        .into_iter()
        .collect(),
    );
    let (subgraph_client_inputs, connector_client_inputs) =
        parse_http_client_inputs(&plugins, &schema, &configuration)?;
    let query_plan_redis = connect_query_plan_redis(&configuration).await?;

    for (_, plugin) in plugins.iter() {
        plugin.activate();
    }

    let query_plan_cache = build_query_plan_cache(&configuration, query_plan_redis);
    let (http_service_factory, connector_http_service_factory) =
        build_http_services(subgraph_client_inputs, connector_client_inputs, &plugins);
    let subgraph_services = build_subgraph_services(&http_service_factory);
    let supergraph_pipeline = build_supergraph_pipeline(
        query_planner_service,
        query_plan_cache,
        schema,
        subgraph_schemas,
        configuration,
        plugins.clone(),
        subgraph_services.into_iter().collect(),
        connector_http_service_factory,
    );

    Ok((plugins, supergraph_pipeline))
}

/// One built serving pipeline: the router service stack plus the schema, plugins, and
/// caches a hot reload needs from it.
#[derive(Clone)]
pub(crate) struct Pipeline {
    pub(crate) schema: Arc<Schema>,
    pub(crate) plugins: Arc<Plugins>,
    in_memory_query_plan_cache: InMemoryQueryPlanCache,
    service: router::BoxCloneService,
    warmup_service: warmup::BoxCloneService,
    persisted_queries: Arc<PersistedQueryExpander>,
    pipeline_handle: Arc<PipelineHandle>,
    /// The configuration used to create this router, stored for hot reload previous config extraction
    pub(crate) configuration: Arc<Configuration>,
}

impl RouterFactory for Pipeline {
    fn create(&self) -> router::BoxCloneService {
        self.service.clone()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.plugins
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }

    fn pipeline_handle(&self) -> Arc<PipelineHandle> {
        self.pipeline_handle.clone()
    }
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        persisted_queries: Arc<PersistedQueryExpander>,
        apq_expander: APQExpander,
        supergraph_service: supergraph::BoxCloneService,
        caching_query_planner: query_planner::CacheBoxCloneService,
        schema: Arc<Schema>,
        plugins: Arc<Plugins>,
        in_memory_query_plan_cache: InMemoryQueryPlanCache,
        query_parsing_service: query_parsing::BoxCloneService,
        configuration: Arc<Configuration>,
    ) -> Self {
        let warmup_service = ServiceBuilder::new()
            .layer(warmup::WarmupParseQueryLayer::new(
                query_parsing_service.clone(),
            ))
            .map_response(drop) // Ignore response
            .service(caching_query_planner)
            .boxed_clone();
        let static_page = StaticPageLayer::new(&configuration);

        // PipelineHandle registers the apollo.router.pipelines up-down counter, so
        // operators can see when old pipelines are still held alive.
        let schema_id = schema.schema_id.to_string();
        let launch_id = schema
            .launch_id
            .as_ref()
            .map(|launch_id| launch_id.to_string());
        let config_hash = configuration.hash();
        let pipeline_handle = PipelineHandle::new(schema_id, launch_id, config_hash);

        let router_service = RouterService::new(
            supergraph_service,
            apq_expander,
            persisted_queries.clone(),
            query_parsing_service,
            schema.clone(),
            &configuration,
            configuration.batching.clone(),
        );

        let service = ServiceBuilder::new()
            .layer(static_page.clone())
            .rust_plugins(plugins.clone(), |plugin, service| {
                plugin.router_service(service)
            })
            .layer(content_negotiation::RouterContentNegotiationLayer::default())
            .service(router_service)
            .boxed_clone();

        Self {
            schema,
            plugins,
            in_memory_query_plan_cache,
            service,
            warmup_service,
            persisted_queries,
            pipeline_handle: Arc::new(pipeline_handle),
            configuration,
        }
    }

    /// Warms the query-plan cache by replaying a configurable sample of previously
    /// cached queries, plus configured persisted queries, through the planner.
    fn warm_up(
        &self,
        previous_cache: Option<InMemoryQueryPlanCache>,
    ) -> impl Future<Output = ()> + Send + use<> {
        // Clone what the future needs up front: the warmup service is not Sync, so a
        // future borrowing self would not be Send.
        let warmup_service = self.warmup_service.clone();
        let persisted_queries = self.persisted_queries.clone();
        let configuration = self.configuration.clone();
        async move {
            warmup::warm_up_query_planner(
                warmup_service,
                &persisted_queries,
                previous_cache,
                configuration.supergraph.query_planning.warmed_up_queries,
                &configuration
                    .persisted_queries
                    .experimental_prewarm_query_plan_cache,
            )
            .await;
        }
    }

    pub(crate) fn previous_cache(&self) -> InMemoryQueryPlanCache {
        self.in_memory_query_plan_cache.clone()
    }
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock_with_configuration(
    mock: tower_test::mock::Mock<supergraph::Request, supergraph::Response>,
    configuration: Arc<Configuration>,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send
+ Clone {
    let (_, schema, plugins, supergraph_pipeline) = crate::TestHarness::builder()
        .configuration(configuration.clone())
        .supergraph_hook(move |_| mock.clone().boxed_clone())
        .build_common()
        .await
        .unwrap();
    let SupergraphPipeline {
        supergraph_service,
        in_memory_query_plan_cache,
        caching_query_planner,
    } = supergraph_pipeline;

    let query_parsing_service = build_query_parsing_service(schema.clone(), configuration.clone());

    let apq_expander = build_apq_expander(
        &configuration,
        connect_apq_redis(&configuration).await.unwrap(),
    );

    Pipeline::new(
        Arc::new(PersistedQueryExpander::new(&configuration).await.unwrap()),
        apq_expander,
        supergraph_service,
        caching_query_planner,
        schema,
        plugins,
        in_memory_query_plan_cache,
        query_parsing_service,
        configuration,
    )
    .create()
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock(
    mock: tower_test::mock::Mock<supergraph::Request, supergraph::Response>,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send
+ Clone {
    from_supergraph_mock_with_configuration(mock, Arc::new(Configuration::default())).await
}

#[cfg(test)]
pub(crate) async fn empty() -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    // empty() discards the handle: the service is never expected to be called, and any
    // call fails with a `Closed` error.
    let (mock, _handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
    from_supergraph_mock_with_configuration(mock, Arc::new(Configuration::default())).await
}
