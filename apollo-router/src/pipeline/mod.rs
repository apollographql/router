//! Construction of the router's serving pipeline from configuration, schema, and license.
//!
//! [`build_pipeline`] runs three phases, each under its own tracing span:
//!
//! - **Acquire** (the [`acquire`](mod@self::acquire) submodule) gathers every resource whose
//!   creation can fail: the telemetry plugin, the federation query planner, the other
//!   plugins, TLS/DNS client material, Redis clients, and the persisted-query manifest.
//! - **Activate** runs every plugin's `activate()` hook. The telemetry hook swaps in global
//!   tracer and meter providers that cannot be rolled back. Nothing after this phase starts
//!   may fail.
//! - **Assemble** (the [`stages`] submodule) builds the caches and service stacks from the
//!   acquired resources, using infallible functions. Each cache registers its gauges in its
//!   constructor; constructing caches after the meter-provider swap binds those gauges to
//!   the provider that serves this pipeline.

use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;

use self::acquire::Acquired;
use self::acquire::acquire;
pub(crate) use self::acquire::connect_apq_redis;
pub(crate) use self::acquire::connect_query_plan_redis;
pub(crate) use self::acquire::create_plugins;
pub(crate) use self::acquire::create_query_planner_service;
// Only tests outside this module call `inject_schema_id`; within the pipeline it is an
// implementation detail of the acquire phase.
#[cfg(test)]
pub(crate) use self::acquire::inject_schema_id;
pub(crate) use self::acquire::parse_http_client_material;
pub(crate) use self::stages::build_apq_expander;
pub(crate) use self::stages::build_http_services;
pub(crate) use self::stages::build_query_plan_cache;
pub(crate) use self::stages::build_supergraph_pipeline;
pub(crate) use self::stages::create_subgraph_services;
pub(crate) use self::stages::query_parsing_service;
use crate::configuration::Configuration;
use crate::plugin::DynPlugin;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::query_planner::warmup;
use crate::services::router::service::RouterCreator;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

mod acquire;
mod stages;
#[cfg(test)]
mod tests;

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
        let query_parsing_service = query_parsing_service(schema.clone(), configuration.clone());

        let (supergraph_service, in_memory_query_plan_cache, caching_query_planner) = {
            let _span = tracing::info_span!("supergraph_creation").entered();
            let (http_service_factory, connector_http_service_factory) = build_http_services(
                subgraph_client_material,
                connector_client_material,
                &plugins,
            );
            let subgraph_services = create_subgraph_services(&http_service_factory);
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
        };

        let router_creator = RouterCreator::new(
            persisted_queries.clone(),
            apq_expander,
            supergraph_service,
            schema,
            plugins,
            in_memory_query_plan_cache,
            query_parsing_service.clone(),
            configuration.clone(),
        );

        let warmup_query_planner_service = ServiceBuilder::new()
            .layer(warmup::WarmupParseQueryLayer::new(query_parsing_service))
            .map_response(drop) // Ignore response
            .service(caching_query_planner)
            .boxed_clone();

        warmup::warm_up_query_planner(
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
