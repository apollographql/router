//! Construction of the router's serving pipeline from configuration, schema, and license.
//!
//! A [`Pipeline`] is one immutable, fully wired instance of the router's request-handling
//! stack, plus the pieces a hot reload needs from it (configuration, schema, plugins, and
//! the in-memory query-plan cache). The state machine builds one at startup and a
//! replacement on every configuration or schema change, via [`build_pipeline`].
//!
//! # The request-time stack
//!
//! A built pipeline is a chain of tower service stacks, one per protocol level. From the
//! outside in:
//!
//! - **Router** — HTTP in, HTTP out. Static pages, each plugin's `router_service` hook,
//!   content negotiation, batch splitting, the HTTP→GraphQL translation, APQ and
//!   persisted-query expansion, and query parsing, dispatching to the supergraph
//!   service.
//! - **Supergraph** — GraphQL request in, GraphQL response out. Content negotiation, each
//!   plugin's `supergraph_service` hook, mutation and operation-limit enforcement, then
//!   `SupergraphService`: introspection and query planning (through the query-plan
//!   cache), and dispatch to the execution service.
//! - **Execution** — executes one query plan. Batch analysis, subscriptions, each
//!   plugin's `execution_service` hook, then `ExecutionService`, whose `FetchService`
//!   routes fetch nodes to subgraphs and connectors.
//! - **Subgraph / connector** — one pre-built stack per subgraph
//!   ([`SubgraphServices`](crate::services::SubgraphServices)) and per connector
//!   (`ConnectorServices` over `ConnectorRequestServices`), with their plugin hooks
//!   (`subgraph_service`, `connector_request_service`) and per-entry buffers, ending in
//!   an HTTP client.
//! - **HTTP client** — one client per subgraph and per connector source: request
//!   batching, response size limits, each plugin's `http_client_service` hook, then the
//!   hyper-based `HttpClientService`.
//!
//! Every one of these stacks is assembled by a `build_*` function in [`stages`], so the
//! full layer composition of the pipeline is legible in that one file.
//!
//! # Construction: acquire → activate → assemble
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
//!   acquired resources, using infallible functions. The query-plan and APQ caches register
//!   their gauges in their constructors; constructing them after the meter-provider swap
//!   binds those gauges to the provider that serves this pipeline.
//!
//! After assemble, [`Pipeline::warm_up`] populates the query-plan cache — from a sample
//! of the previous pipeline's cache on a reload, and from persisted queries when
//! configured — before [`build_pipeline`] returns the pipeline to serve traffic.
//!
//! # Hot reload
//!
//! On a reload, [`build_pipeline`] receives the previous pipeline's configuration and
//! in-memory query-plan cache. The telemetry bootstrap is skipped — the global tracer and
//! meter providers from first boot stay installed until the telemetry plugin's
//! `activate()` swaps in new ones — and warm-up draws its queries from the previous
//! cache. Plugin instantiation, planner creation, Redis connects, and stack assembly all
//! run from scratch, so the old pipeline keeps serving unchanged until the state machine
//! swaps the new one in.
//!
//! Plugin construction order (and with it, hook order at every stack above) is defined
//! once, in [`plugins`] ([`create_plugins`]).
//!
//! `dev-docs/pipeline-construction.md` covers the design rationale, the failure-model
//! table, and the instrumentation.

use std::sync::Arc;

#[cfg(test)]
use futures::future::BoxFuture;
use multimap::MultiMap;
use tower::BoxError;
#[cfg(test)]
use tower::Service;
#[cfg(test)]
use tower::ServiceBuilder;
#[cfg(test)]
use tower::ServiceExt;
use tracing::Instrument;

use self::acquire::Acquired;
use self::acquire::acquire;
use self::acquire::maybe_bootstrap_telemetry;
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
pub(crate) use self::stages::build_router_service;
pub(crate) use self::stages::build_subgraph_services;
pub(crate) use self::stages::build_supergraph_pipeline;
use self::stages::build_warmup_service;
#[cfg(test)]
pub(crate) use self::stages::wrap_subgraph_services;
use crate::Endpoint;
use crate::ListenAddr;
use crate::configuration::Configuration;
use crate::plugin::DynPlugin;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::warmup;
use crate::router_factory::RouterFactory;
use crate::router_factory::STARTING_SPAN_NAME;
use crate::services::Plugins;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineHandle;
#[cfg(test)]
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
    // Bootstrap telemetry before creating any spans: on first boot the global tracer
    // provider is still the no-op bootstrap provider, so a span created earlier would
    // never be exported and every span under it would arrive as an orphan.
    let bootstrap_telemetry_plugin = maybe_bootstrap_telemetry(
        &configuration,
        &schema,
        &license,
        previous_config.as_deref(),
    )
    .await?;

    async {
        let acquired = acquire(
            &configuration,
            &schema,
            previous_config,
            extra_plugins,
            license,
            bootstrap_telemetry_plugin,
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
    .instrument(tracing::info_span!(STARTING_SPAN_NAME))
    .await
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
/// Call after [`activate`]: the query-plan and APQ caches built here register their
/// gauges against the meter provider that activation installed.
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
        let (subgraph_http_services, connector_http_services) =
            build_http_services(subgraph_client_inputs, connector_client_inputs, &plugins);
        let subgraph_services =
            build_subgraph_services(subgraph_http_services, &plugins, &configuration);
        build_supergraph_pipeline(
            query_planner_service,
            query_plan_cache,
            schema.clone(),
            subgraph_schemas,
            configuration.clone(),
            plugins.clone(),
            subgraph_services,
            connector_http_services,
        )
    });

    let warmup_service = build_warmup_service(query_parsing_service.clone(), caching_query_planner);

    let service = build_router_service(
        supergraph_service,
        apq_expander,
        persisted_queries.clone(),
        query_parsing_service,
        schema.clone(),
        &configuration,
        plugins.clone(),
    );

    // PipelineHandle registers the apollo.router.pipelines up-down counter, so
    // operators can see when old pipelines are still held alive.
    let pipeline_handle = PipelineHandle::new(
        schema.schema_id.to_string(),
        schema.launch_id.as_ref().map(|id| id.to_string()),
        configuration.hash(),
    );

    Pipeline {
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

/// Builds an activated supergraph service for [`TestHarness`](crate::TestHarness),
/// without the telemetry early-activation, APQ, persisted queries, warm-up, and router
/// service that [`build_pipeline`] adds around it.
pub(crate) async fn build_supergraph_for_test_harness(
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
    let (subgraph_http_services, connector_http_services) =
        build_http_services(subgraph_client_inputs, connector_client_inputs, &plugins);
    let subgraph_services =
        build_subgraph_services(subgraph_http_services, &plugins, &configuration);
    let supergraph_pipeline = build_supergraph_pipeline(
        query_planner_service,
        query_plan_cache,
        schema,
        subgraph_schemas,
        configuration,
        plugins.clone(),
        subgraph_services,
        connector_http_services,
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
    /// The configuration this pipeline was built from.
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
    use crate::layers::ServiceBuilderExt as _;

    let (_, schema, plugins, supergraph_pipeline) = crate::TestHarness::builder()
        .configuration(configuration.clone())
        // Buffer the mock so it stays permanently ready: a tower_test mock fails
        // `poll_ready` once its handle is dropped (as in [`empty`]), which would fail
        // requests before they reach the router layers under test.
        .supergraph_hook(move |_| {
            ServiceBuilder::new()
                .buffered()
                .service(mock.clone().boxed_clone())
                .boxed_clone()
        })
        .build_common()
        .await
        .unwrap();
    let SupergraphPipeline {
        supergraph_service, ..
    } = supergraph_pipeline;

    let query_parsing_service = build_query_parsing_service(schema.clone(), configuration.clone());

    let apq_expander = build_apq_expander(
        &configuration,
        connect_apq_redis(&configuration).await.unwrap(),
    );

    build_router_service(
        supergraph_service,
        apq_expander,
        Arc::new(PersistedQueryExpander::new(&configuration).await.unwrap()),
        query_parsing_service,
        schema,
        &configuration,
        plugins,
    )
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
