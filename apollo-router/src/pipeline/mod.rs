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
//! - **Router** — HTTP in, HTTP out: turns HTTP requests into GraphQL requests.
//! - **Supergraph** — GraphQL request in, GraphQL response out: plans the operation.
//! - **Execution** — executes one query plan, routing its fetch nodes to subgraphs and
//!   connectors.
//! - **Subgraph / connector** — one pre-built stack per subgraph
//!   ([`SubgraphServices`](crate::services::SubgraphServices)) and per connector
//!   (`ConnectorServices` over `ConnectorRequestServices`).
//! - **HTTP client** — one client per subgraph and per connector source.
//!
//! Each stack applies that stage's plugin hooks. Every stack is assembled by a `build_*`
//! function in [`stages`], so the full layer composition of the pipeline is legible in
//! that one file — the service-builder chains there are the authoritative description of
//! what runs where.
//!
//! # Construction: prepare → activate → apply
//!
//! [`build_pipeline`] runs two phases around the point of no return:
//!
//! - **Prepare** (`prepare_pipeline` span) is everything fallible and everything slow:
//!   acquiring resources (the [`acquire`](mod@self::acquire) submodule — the telemetry
//!   plugin, the federation query planner, the other plugins, TLS/DNS client inputs,
//!   Redis clients, and the persisted-query manifest), building the query-planning
//!   pipeline, and warming the plan cache from a sample of the previous pipeline's cache
//!   and, when configured, persisted queries. On a reload all of this runs while the
//!   previous pipeline — and its telemetry — is still fully live.
//! - **Activate** runs every plugin's `activate()` hook, between the two phases. The
//!   telemetry hook swaps in global tracer and meter providers that cannot be rolled
//!   back; nothing after this may fail, and everything after it is fast.
//! - **Apply** (`apply_pipeline` span) assembles the serving stacks (the [`stages`]
//!   submodule) from the prepared resources, using infallible functions, and re-registers
//!   the plan-cache gauges when activation swapped the meter provider.
//!
//! # Hot reload
//!
//! On a reload, [`build_pipeline`] receives the previous pipeline's configuration and
//! in-memory query-plan cache. The telemetry bootstrap is skipped — the global tracer and
//! meter providers from first boot stay installed until the telemetry plugin's
//! `activate()` swaps in new ones — and warm-up draws its queries from the previous
//! cache. Plugin instantiation, planner creation, Redis connects, and stack assembly all
//! run from scratch, so the old pipeline keeps serving unchanged, with working telemetry,
//! until the fast apply phase swaps the new one in.
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
use tower::ServiceExt;
use tracing::Instrument;

use self::acquire::Acquired;
use self::acquire::acquire;
pub(crate) use self::acquire::connect_apq_redis;
pub(crate) use self::acquire::connect_query_plan_redis;
pub(crate) use self::acquire::create_query_planner;
use self::acquire::maybe_bootstrap_telemetry;
pub(crate) use self::acquire::parse_http_client_inputs;
pub(crate) use self::plugins::create_plugins;
pub(crate) use self::stages::build_apq_expander;
pub(crate) use self::stages::build_caching_query_planner;
#[cfg(test)]
pub(crate) use self::stages::build_http_client_service;
pub(crate) use self::stages::build_http_services;
pub(crate) use self::stages::build_query_parsing_service;
pub(crate) use self::stages::build_query_plan_cache;
pub(crate) use self::stages::build_query_planner_service;
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
use crate::plugins::telemetry::reload::otel::apollo_opentelemetry_initialized;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::warmup;
use crate::router_factory::RouterFactory;
use crate::services::Plugins;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineHandle;
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

    // On a hot reload with live telemetry, [`activate`] swaps the meter provider,
    // discarding the gauge callbacks the query-plan cache registered during prepare.
    // (On first boot the bootstrap above already installed the provider, and the
    // telemetry plugin's activation is a one-shot no-op at the boundary.)
    let activation_swaps_providers =
        bootstrap_telemetry_plugin.is_none() && apollo_opentelemetry_initialized();

    let prepared = prepare_pipeline(
        &configuration,
        &schema,
        previous_config,
        previous_cache,
        extra_plugins,
        license,
        bootstrap_telemetry_plugin,
    )
    .instrument(tracing::info_span!("prepare_pipeline"))
    .await?;

    activate(&prepared.plugins);

    let pipeline = tracing::info_span!("apply_pipeline")
        .in_scope(|| apply_pipeline(prepared, configuration, schema, activation_swaps_providers));

    Ok(pipeline)
}

/// Everything the prepare phase hands to [`apply_pipeline`]: the acquired resources plus
/// the already-warm query-planning pipeline.
struct Prepared {
    plugins: Arc<Plugins>,
    subgraph_schemas: Arc<crate::query_planner::SubgraphSchemas>,
    http_client_inputs: acquire::HttpClientInputsMaps,
    apq_redis: Option<crate::cache::redis::RedisCacheStorage>,
    persisted_queries: Arc<PersistedQueryExpander>,
    query_parsing_service: crate::services::query_parsing::BoxCloneService,
    query_plan_cache: crate::query_planner::QueryPlanCache,
    caching_query_planner: crate::services::query_planner::CacheBoxCloneService,
}

/// The fallible, slow half of pipeline construction: acquires every resource, builds
/// the query-planning pipeline, and warms the plan cache. Runs while the previous
/// pipeline (and its telemetry) is still fully live, so a reload's slow work happens
/// before the point of no return.
async fn prepare_pipeline(
    configuration: &Arc<Configuration>,
    schema: &Arc<Schema>,
    previous_config: Option<Arc<Configuration>>,
    previous_cache: Option<InMemoryQueryPlanCache>,
    extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
    license: Arc<LicenseState>,
    bootstrap_telemetry_plugin: Option<Box<dyn DynPlugin>>,
) -> Result<Prepared, BoxError> {
    let Acquired {
        query_planner,
        subgraph_schemas,
        plugins,
        http_client_inputs,
        query_plan_redis,
        apq_redis,
        persisted_queries,
    } = acquire(
        configuration,
        schema,
        previous_config,
        extra_plugins,
        license,
        bootstrap_telemetry_plugin,
    )
    .instrument(tracing::info_span!("acquire"))
    .await?;

    let query_parsing_service = build_query_parsing_service(schema.clone(), configuration.clone());
    let query_plan_cache = build_query_plan_cache(configuration, query_plan_redis);
    let query_planner_service =
        build_query_planner_service(schema.clone(), configuration.clone(), query_planner);
    let caching_query_planner = build_caching_query_planner(
        query_planner_service,
        query_plan_cache.clone(),
        schema.clone(),
        subgraph_schemas.clone(),
        configuration,
    );

    let warmup_service =
        build_warmup_service(query_parsing_service.clone(), caching_query_planner.clone());
    warmup::warm_up_query_planner(
        warmup_service,
        &persisted_queries,
        previous_cache,
        configuration.supergraph.query_planning.warmed_up_queries,
        &configuration
            .persisted_queries
            .experimental_prewarm_query_plan_cache,
    )
    .instrument(tracing::info_span!("warmup"))
    .await;

    Ok(Prepared {
        plugins,
        subgraph_schemas,
        http_client_inputs,
        apq_redis,
        persisted_queries,
        query_parsing_service,
        query_plan_cache,
        caching_query_planner,
    })
}

/// The point of no return: activating the telemetry plugin swaps in global tracer and
/// meter providers that cannot be rolled back. From here on the pipeline must go live,
/// so everything after this call is infallible and fast.
fn activate(plugins: &Plugins) {
    tracing::info_span!("activate").in_scope(|| {
        for (_, plugin) in plugins.iter() {
            plugin.activate();
        }
    })
}

/// The infallible, fast half of pipeline construction: assembles the serving stacks
/// from the prepared resources.
///
/// Call after [`activate`]: anything assembled here assumes that telemetry is active.
fn apply_pipeline(
    prepared: Prepared,
    configuration: Arc<Configuration>,
    schema: Arc<Schema>,
    activation_swapped_providers: bool,
) -> Pipeline {
    let Prepared {
        plugins,
        subgraph_schemas,
        http_client_inputs,
        apq_redis,
        persisted_queries,
        query_parsing_service,
        query_plan_cache,
        caching_query_planner,
    } = prepared;

    if activation_swapped_providers {
        // The plan cache was built during prepare so warm-up could populate it; the
        // provider swap discarded the gauges it registered at construction.
        query_plan_cache.register_gauges();
    }

    let apq_expander = build_apq_expander(&configuration, apq_redis);

    let supergraph_service = tracing::info_span!("supergraph_creation").in_scope(|| {
        let (subgraph_http_services, connector_http_services) =
            build_http_services(http_client_inputs, &plugins);
        let subgraph_services =
            build_subgraph_services(subgraph_http_services, &plugins, &configuration);
        build_supergraph_pipeline(
            caching_query_planner,
            schema.clone(),
            subgraph_schemas,
            configuration.clone(),
            plugins.clone(),
            subgraph_services,
            connector_http_services,
        )
    });

    let service = build_router_service(
        supergraph_service,
        apq_expander,
        persisted_queries,
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
        in_memory_query_plan_cache: query_plan_cache.in_memory_cache(),
        service,
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
) -> Result<(Arc<Plugins>, supergraph::BoxCloneService), BoxError> {
    let (query_planner, subgraph_schemas) = create_query_planner(&schema, &configuration)?;
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
    let http_client_inputs = parse_http_client_inputs(&plugins, &schema, &configuration)?;
    let query_plan_redis = connect_query_plan_redis(&configuration).await?;

    for (_, plugin) in plugins.iter() {
        plugin.activate();
    }

    let query_planner_service =
        build_query_planner_service(schema.clone(), configuration.clone(), query_planner);
    let query_plan_cache = build_query_plan_cache(&configuration, query_plan_redis);
    let caching_query_planner = build_caching_query_planner(
        query_planner_service,
        query_plan_cache.clone(),
        schema.clone(),
        subgraph_schemas.clone(),
        &configuration,
    );
    let (subgraph_http_services, connector_http_services) =
        build_http_services(http_client_inputs, &plugins);
    let subgraph_services =
        build_subgraph_services(subgraph_http_services, &plugins, &configuration);
    let supergraph_service = build_supergraph_pipeline(
        caching_query_planner,
        schema,
        subgraph_schemas,
        configuration,
        plugins.clone(),
        subgraph_services,
        connector_http_services,
    );

    Ok((plugins, supergraph_service))
}

/// One built serving pipeline: the router service stack plus the schema, plugins, and
/// caches a hot reload needs from it.
#[derive(Clone)]
pub(crate) struct Pipeline {
    pub(crate) schema: Arc<Schema>,
    pub(crate) plugins: Arc<Plugins>,
    in_memory_query_plan_cache: InMemoryQueryPlanCache,
    service: router::BoxCloneService,
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
    let (_, schema, plugins, supergraph_service) = crate::TestHarness::builder()
        .configuration(configuration.clone())
        .supergraph_hook(move |_| mock.clone().boxed_clone())
        .build_common()
        .await
        .unwrap();
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
    let (mock, handle) = tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
    // The supergraph service must stay ready — these tests exercise router-layer
    // rejections — but must never be called.
    crate::plugin::test::allow_and_assert_never_called(handle);
    from_supergraph_mock_with_configuration(mock, Arc::new(Configuration::default())).await
}
