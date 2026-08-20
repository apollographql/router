//! Test harness and mocks for the Apollo Router.

use std::collections::HashMap;
use std::collections::HashSet;
use std::default::Default;
use std::str::FromStr;
use std::sync::Arc;

use serde::de::Error as DeserializeError;
use serde::ser::Error as SerializeError;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_http::trace::MakeSpan;
use tracing_futures::Instrument;

use crate::AllowedFeature;
use crate::axum_factory::utils::PropagatingMakeSpan;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::graphql;
use crate::pipeline::Pipeline;
use crate::pipeline::SupergraphPipeline;
use crate::pipeline::build_apq_expander;
use crate::pipeline::build_http_services;
use crate::pipeline::build_query_parsing_service;
use crate::pipeline::build_query_plan_cache;
use crate::pipeline::build_subgraph_services;
use crate::pipeline::build_supergraph_pipeline;
use crate::pipeline::connect_apq_redis;
use crate::pipeline::connect_query_plan_redis;
use crate::pipeline::create_plugins;
use crate::pipeline::create_query_planner_service;
use crate::pipeline::parse_http_client_material;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::PluginUnstable;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::canned;
use crate::plugins::telemetry::reload::otel::init_telemetry;
use crate::query_planner::InMemoryQueryPlanCache;
use crate::router_factory::RouterFactory;
use crate::services::Plugins;
use crate::services::execution;
use crate::services::layers::persisted_queries::PersistedQueryExpander;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseLimits;
use crate::uplink::license_enforcement::LicenseState;

/// Mocks for services the Apollo Router must integrate with.
pub mod mocks;

#[cfg(test)]
pub(crate) mod http_client;

#[cfg(any(test, feature = "snapshot"))]
pub(crate) mod http_snapshot;

/// Builder for the part of an Apollo Router that handles GraphQL requests, as a [`tower::Service`].
///
/// This allows tests, benchmarks, etc
/// to manipulate request and response objects in memory
/// without going over the network on the supergraph side.
///
/// On the subgraph side, this test harness never makes network requests to subgraphs
/// unless [`with_subgraph_network_requests`][Self::with_subgraph_network_requests] is called.
///
/// Compared to running a full [`RouterHttpServer`][crate::RouterHttpServer],
/// this test harness is lacking:
///
/// * Custom endpoints from plugins
/// * The health check endpoint
/// * CORS (FIXME: should this include CORS?)
/// * HTTP compression
///
/// Example making a single request:
///
/// ```
/// use apollo_router::services::supergraph;
/// use apollo_router::TestHarness;
/// use tower::util::ServiceExt;
///
/// # #[tokio::main] async fn main() -> Result<(), tower::BoxError> {
/// let config = serde_json::json!({"supergraph": { "introspection": false }});
/// let request = supergraph::Request::fake_builder()
///     // Request building here
///     .build()
///     .unwrap();
/// let response = TestHarness::builder()
///     .configuration_json(config)?
///     .build_router()
///     .await?
///     .oneshot(request.try_into().unwrap())
///     .await?
///     .next_response()
///     .await
///     .unwrap();
/// # Ok(()) }
/// ```
pub struct TestHarness<'a> {
    schema: Option<&'a str>,
    configuration: Option<Arc<Configuration>>,
    extra_plugins: Vec<(String, Box<dyn DynPlugin>)>,
    subgraph_network_requests: bool,
    license: Option<Arc<LicenseState>>,
}

// Not using buildstructor because `extra_plugin` has non-trivial signature and behavior
impl<'a> TestHarness<'a> {
    /// Creates a new builder.
    pub fn builder() -> Self {
        Self {
            schema: None,
            configuration: None,
            extra_plugins: Vec::new(),
            subgraph_network_requests: false,
            license: None,
        }
    }

    /// Specifies the logging level. Note that this function may not be called more than once.
    /// log_level is in RUST_LOG format.
    pub fn log_level(self, log_level: &'a str) -> Self {
        // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
        let log_level = format!("{log_level},salsa=error");
        init_telemetry(&log_level).expect("failed to setup logging");
        self
    }

    /// Specifies the logging level. Note that this function will silently fail if called more than once.
    /// log_level is in RUST_LOG format.
    pub fn try_log_level(self, log_level: &'a str) -> Self {
        // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
        let log_level = format!("{log_level},salsa=error");
        let _ = init_telemetry(&log_level);
        self
    }

    /// Specifies the (static) supergraph schema definition.
    ///
    /// Panics if called more than once.
    ///
    /// If this isn’t called, a default “canned” schema is used.
    /// It can be found in the Router repository at `apollo-router/testing_schema.graphql`.
    /// In that case, subgraph responses are overridden with some “canned” data.
    pub fn schema(mut self, schema: &'a str) -> Self {
        assert!(self.schema.is_none(), "schema was specified twice");
        self.schema = Some(schema);
        self
    }

    /// Specifies the (static) router configuration.
    pub fn configuration(mut self, configuration: Arc<Configuration>) -> Self {
        assert!(
            self.configuration.is_none(),
            "configuration was specified twice"
        );
        self.configuration = Some(configuration);
        self
    }

    /// Specifies the (static) router configuration as a JSON value,
    /// such as from the `serde_json::json!` macro.
    pub fn configuration_json(
        self,
        configuration: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        // Convert from a json Value to yaml str to Configuration so that we can ensure we validate
        // and populate the Configuration's validated_yaml attribute
        let yaml = serde_yaml::to_string(&configuration).map_err(SerializeError::custom)?;
        let configuration: Configuration =
            Configuration::from_str(&yaml).map_err(DeserializeError::custom)?;
        Ok(self.configuration(Arc::new(configuration)))
    }

    /// Specifies the (static) router configuration as a YAML string
    pub fn configuration_yaml(self, configuration: &'a str) -> Result<Self, ConfigurationError> {
        let configuration: Configuration = Configuration::from_str(configuration)?;
        Ok(self.configuration(Arc::new(configuration)))
    }

    /// Specifies the (static) license.
    ///
    /// Panics if called more than once.
    ///
    /// If this isn't called, the default license is used.
    pub fn license_from_allowed_features(mut self, allowed_features: Vec<AllowedFeature>) -> Self {
        assert!(self.license.is_none(), "license was specified twice");
        self.license = Some(Arc::new(LicenseState::Licensed {
            limits: {
                Some(
                    LicenseLimits::builder()
                        .allowed_features(HashSet::from_iter(allowed_features))
                        .build(),
                )
            },
        }));
        self
    }

    /// Adds an extra, already instantiated plugin.
    ///
    /// May be called multiple times.
    /// These extra plugins are added after plugins specified in configuration.
    pub fn extra_plugin<P: Plugin>(mut self, plugin: P) -> Self {
        let type_id = std::any::TypeId::of::<P>();
        let name = match crate::plugin::plugins().find(|factory| factory.type_id == type_id) {
            Some(factory) => factory.name.clone(),
            None => format!(
                "extra_plugins.{}.{}",
                self.extra_plugins.len(),
                std::any::type_name::<P>(),
            ),
        };

        self.extra_plugins.push((name, plugin.into()));
        self
    }

    /// Adds an extra, already instantiated unstable plugin.
    ///
    /// May be called multiple times.
    /// These extra plugins are added after plugins specified in configuration.
    pub fn extra_unstable_plugin<P: PluginUnstable>(mut self, plugin: P) -> Self {
        let type_id = std::any::TypeId::of::<P>();
        let name = match crate::plugin::plugins().find(|factory| factory.type_id == type_id) {
            Some(factory) => factory.name.clone(),
            None => format!(
                "extra_plugins.{}.{}",
                self.extra_plugins.len(),
                std::any::type_name::<P>(),
            ),
        };

        self.extra_plugins.push((name, Box::new(plugin)));
        self
    }

    /// Adds an extra, already instantiated private plugin.
    ///
    /// May be called multiple times.
    /// These extra plugins are added after plugins specified in configuration.
    #[allow(dead_code)]
    pub(crate) fn extra_private_plugin<P: PluginPrivate>(mut self, plugin: P) -> Self {
        let type_id = std::any::TypeId::of::<P>();
        let name = match crate::plugin::plugins().find(|factory| factory.type_id == type_id) {
            Some(factory) => factory.name.clone(),
            None => format!(
                "extra_plugins.{}.{}",
                self.extra_plugins.len(),
                std::any::type_name::<P>(),
            ),
        };

        self.extra_plugins.push((name, Box::new(plugin)));
        self
    }

    /// Adds a callback-based hook similar to [`Plugin::router_service`]
    pub fn router_hook(
        self,
        callback: impl Fn(router::BoxCloneService) -> router::BoxCloneService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(RouterServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::supergraph_service`]
    pub fn supergraph_hook(
        self,
        callback: impl Fn(supergraph::BoxCloneService) -> supergraph::BoxCloneService
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.extra_plugin(SupergraphServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::execution_service`]
    pub fn execution_hook(
        self,
        callback: impl Fn(execution::BoxCloneService) -> execution::BoxCloneService
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.extra_plugin(ExecutionServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::subgraph_service`]
    pub fn subgraph_hook(
        self,
        callback: impl Fn(&str, subgraph::BoxCloneService) -> subgraph::BoxCloneService
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.extra_plugin(SubgraphServicePlugin(callback))
    }

    /// Enables this test harness to make network requests to subgraphs.
    ///
    /// If this is not called, all subgraph requests get an empty response by default
    /// (unless [`schema`][Self::schema] is also not called).
    /// This behavior can be changed by implementing [`Plugin::subgraph_service`]
    /// on a plugin given to [`extra_plugin`][Self::extra_plugin].
    pub fn with_subgraph_network_requests(mut self) -> Self {
        self.subgraph_network_requests = true;
        self
    }

    pub(crate) async fn build_common(
        self,
    ) -> Result<
        (
            Arc<Configuration>,
            Arc<Schema>,
            Arc<Plugins>,
            supergraph::BoxCloneService,
            InMemoryQueryPlanCache,
        ),
        BoxError,
    > {
        let mut config = self.configuration.unwrap_or_default();
        let has_legacy_mock_subgraphs_plugin = self.extra_plugins.iter().any(|(_, dyn_plugin)| {
            dyn_plugin.name() == *crate::plugins::mock_subgraphs::PLUGIN_NAME
        });
        if self.schema.is_none() && !has_legacy_mock_subgraphs_plugin {
            Arc::make_mut(&mut config)
                .apollo_plugins
                .plugins
                .entry("experimental_mock_subgraphs")
                .or_insert_with(canned::mock_subgraphs);
        }
        if !self.subgraph_network_requests {
            Arc::make_mut(&mut config)
                .apollo_plugins
                .plugins
                .entry("experimental_mock_subgraphs")
                .or_insert(serde_json::json!({}));
        }
        let canned_schema = include_str!("../testing_schema.graphql");
        let schema = self.schema.unwrap_or(canned_schema);
        let schema = Arc::new(Schema::parse(schema, &config)?);
        // Default to using an unrestricted license
        let license = self.license.unwrap_or(Arc::new(LicenseState::Licensed {
            limits: Default::default(),
        }));

        // Compose the acquire, activate, and assemble steps from `crate::pipeline` directly:
        // tests need an activated supergraph service without the router service, APQ expander,
        // and warm-up that `build_pipeline` adds on top.
        let (query_planner_service, subgraph_schemas) =
            create_query_planner_service(&schema, &config)?;
        let plugins: Arc<Plugins> = Arc::new(
            create_plugins(
                &config,
                &schema,
                subgraph_schemas.clone(),
                None,
                Some(self.extra_plugins),
                license,
                None,
            )
            .instrument(tracing::info_span!("plugins"))
            .await?
            .into_iter()
            .collect(),
        );
        let (subgraph_client_material, connector_client_material) =
            parse_http_client_material(&plugins, &schema, &config)?;
        let query_plan_redis = connect_query_plan_redis(&config).await?;

        for (_, plugin) in plugins.iter() {
            plugin.activate();
        }

        let query_plan_cache = build_query_plan_cache(&config, query_plan_redis);
        let (http_service_factory, connector_http_service_factory) = build_http_services(
            subgraph_client_material,
            connector_client_material,
            &plugins,
        );
        let subgraph_services = build_subgraph_services(&http_service_factory);
        let SupergraphPipeline {
            supergraph_service,
            in_memory_query_plan_cache,
            ..
        } = build_supergraph_pipeline(
            query_planner_service,
            query_plan_cache,
            schema.clone(),
            subgraph_schemas,
            config.clone(),
            plugins.clone(),
            subgraph_services.into_iter().collect(),
            connector_http_service_factory,
        );

        Ok((
            config,
            schema,
            plugins,
            supergraph_service,
            in_memory_query_plan_cache,
        ))
    }

    /// Builds the supergraph service
    pub async fn build_supergraph(self) -> Result<supergraph::BoxCloneService, BoxError> {
        let (config, schema, _plugins, supergraph_service, _in_memory_query_plan_cache) =
            self.build_common().await?;

        Ok(tower::service_fn(move |request: supergraph::Request| {
            let router = supergraph_service.clone();

            // The supergraph service expects a ParsedDocument in the context. In the real world,
            // that is always populated by the router service. For the testing harness, however,
            // tests normally craft a supergraph request manually, and it's inconvenient to
            // manually populate the ParsedDocument. Instead of doing it many different ways
            // over and over in different tests, we simulate that part of the router service here.
            let body = request.supergraph_request.body();
            // If we don't have a query we definitely won't have a parsed document.
            if let Some(query_str) = body.query.as_deref() {
                let operation_name = body.operation_name.as_deref();
                if !request.context.extensions().with_lock(|lock| {
                    lock.contains_key::<crate::services::query_parsing::ParsedDocument>()
                }) {
                    let doc = crate::spec::Query::parse_document(
                        query_str,
                        operation_name,
                        &schema,
                        &config,
                    )
                    .expect("parse error in test");
                    request.context.extensions().with_lock(|lock| {
                        lock.insert::<crate::services::query_parsing::ParsedDocument>(doc)
                    });
                }
            }

            async move { router.oneshot(request).await }
        })
        .boxed_clone())
    }

    /// Builds the router service
    pub async fn build_router(self) -> Result<router::BoxCloneService, BoxError> {
        let (config, schema, plugins, supergraph_service, in_memory_query_plan_cache) =
            self.build_common().await?;

        let query_parsing_service = build_query_parsing_service(schema.clone(), config.clone());

        let apq_expander = build_apq_expander(&config, connect_apq_redis(&config).await?);
        let pipeline = Pipeline::new(
            Arc::new(PersistedQueryExpander::new(&config).await.unwrap()),
            apq_expander,
            supergraph_service,
            schema,
            plugins,
            in_memory_query_plan_cache,
            query_parsing_service,
            config.clone(),
        );

        Ok(tower::service_fn(move |request: router::Request| {
            let router = ServiceBuilder::new().service(pipeline.create()).boxed();
            let span = PropagatingMakeSpan {
                license: Default::default(),
            }
            .make_span(&request.router_request);
            async move { router.oneshot(request).await }.instrument(span)
        })
        .boxed_clone())
    }

    /// Build the HTTP service
    pub async fn build_http_service(self) -> Result<HttpService, BoxError> {
        use crate::axum_factory::ListenAddrAndRouter;
        use crate::axum_factory::axum_http_server_factory::make_axum_router;
        use crate::axum_factory::utils::connection_router_service;

        let (config, schema, plugins, supergraph_service, in_memory_query_plan_cache) =
            self.build_common().await?;

        let query_parsing_service = build_query_parsing_service(schema.clone(), config.clone());

        let apq_expander = build_apq_expander(&config, connect_apq_redis(&config).await?);
        let pipeline = Pipeline::new(
            Arc::new(PersistedQueryExpander::new(&config).await.unwrap()),
            apq_expander,
            supergraph_service,
            schema,
            plugins,
            in_memory_query_plan_cache,
            query_parsing_service,
            config.clone(),
        );

        let web_endpoints = pipeline.web_endpoints();

        let routers = make_axum_router(
            &config,
            web_endpoints,
            Arc::new(LicenseState::Licensed {
                limits: Default::default(),
            }),
        )?;
        let ListenAddrAndRouter(_listener, router) = routers.main;

        // The router reads its pipeline from a request extension, which the server factory
        // populates. Add the same extension here so the returned service is callable.
        let router_service = connection_router_service(pipeline.create());
        let router = ServiceBuilder::new()
            .layer(tower_http::add_extension::AddExtensionLayer::new(
                router_service,
            ))
            .service(router);
        Ok(router.boxed())
    }
}

/// An HTTP-level service, as would be given to Hyper’s server
pub type HttpService = tower::util::BoxService<
    http::Request<crate::services::router::Body>,
    http::Response<axum::body::Body>,
    std::convert::Infallible,
>;

struct RouterServicePlugin<F>(F);
struct SupergraphServicePlugin<F>(F);
struct ExecutionServicePlugin<F>(F);
struct SubgraphServicePlugin<F>(F);

#[async_trait::async_trait]
impl<F> Plugin for RouterServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(router::BoxCloneService) -> router::BoxCloneService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn router_service(&self, service: router::BoxCloneService) -> router::BoxCloneService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for SupergraphServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(supergraph::BoxCloneService) -> supergraph::BoxCloneService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for ExecutionServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(execution::BoxCloneService) -> execution::BoxCloneService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn execution_service(&self, service: execution::BoxCloneService) -> execution::BoxCloneService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for SubgraphServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(&str, subgraph::BoxCloneService) -> subgraph::BoxCloneService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        (self.0)(subgraph_name, service)
    }
}

/// a list of subgraphs with pregenerated responses
#[derive(Default, Clone)]
pub struct MockedSubgraphs(pub(crate) HashMap<&'static str, MockSubgraph>);

impl MockedSubgraphs {
    /// adds a mocked subgraph to the list
    pub fn insert(&mut self, name: &'static str, subgraph: MockSubgraph) {
        self.0.insert(name, subgraph);
    }
}

#[async_trait::async_trait]
impl Plugin for MockedSubgraphs {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        default: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        self.0
            .get(subgraph_name)
            .map(|service| service.clone().boxed_clone())
            .unwrap_or(default)
    }
}

// This function takes a valid request and duplicates it (optionally, with a new operation
// name) to create an array (batch) request.
//
// Note: It's important to make the operation name different to prevent race conditions in testing
// where various tests assume the presence (or absence) of a test span.
//
// Detailed Explanation
//
// A batch sends a series of requests concurrently through a router. If we
// simply duplicate the request, then there is significant chance that spans such as
// "parse_query" won't appear because the document has already been parsed and is now in a cache.
//
// To explicitly avoid this, we add an operation name which will force the router to re-parse the
// document since operation name is part of the parsed document cache key.
//
// This has been a significant cause of racy/flaky tests in the past.

///
/// Convert a graphql request into a batch of requests
///
/// This is helpful for testing batching functionality.
/// Given a GraphQL request, generate an array containing the request and it's duplicate.
///
/// If an op_from_to is supplied, this will modify the duplicated request so that it uses the new
/// operation name.
///
pub fn make_fake_batch(
    input: http::Request<graphql::Request>,
    op_from_to: Option<(&str, &str)>,
) -> http::Request<crate::services::router::Body> {
    input.map(|req| {
        // Modify the request so that it is a valid array of requests.
        let mut new_req = req.clone();

        // If we were given an op_from_to, then try to modify the query to update the operation
        // name from -> to.
        // If our request doesn't have an operation name or we weren't given an op_from_to,
        // just duplicate the request as is.
        if let Some((from, to)) = op_from_to
            && let Some(operation_name) = &req.operation_name
            && operation_name == from
        {
            new_req.query = req.query.clone().map(|q| q.replace(from, to));
            new_req.operation_name = Some(to.to_string());
        }

        let mut json_bytes_req = serde_json::to_vec(&req).unwrap();
        let mut json_bytes_new_req = serde_json::to_vec(&new_req).unwrap();

        let mut result = vec![b'['];
        result.append(&mut json_bytes_req);
        result.push(b',');
        result.append(&mut json_bytes_new_req);
        result.push(b']');
        router::body::from_bytes(result)
    })
}

#[tokio::test]
async fn test_intercept_subgraph_network_requests() {
    use futures::StreamExt;
    let request = crate::services::supergraph::Request::canned_builder()
        .build()
        .unwrap();
    let response = TestHarness::builder()
        .schema(include_str!("../testing_schema.graphql"))
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            }
        }))
        .unwrap()
        .build_router()
        .await
        .unwrap()
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "topProducts": null
      },
      "errors": [
        {
          "message": "subgraph mock not configured",
          "path": [],
          "extensions": {
            "code": "SUBGRAPH_MOCK_NOT_CONFIGURED",
            "service": "products"
          }
        }
      ]
    }
    "###);
}

/// This module should be used in place of the `::tracing_test::traced_test` macro,
/// which instantiates a global subscriber via a `OnceLock`, causing test failures.
///
/// # Examples
///
/// ```rust
/// use crate::test_harness::tracing_test;
/// fn test_logs_are_captured() {
///     let _guard = tracing_test::dispatcher_guard();
///
///     // explicit call, but this could also be a router call etc
///     tracing::info!("hello world");
///
///     assert!(tracing_test::logs_contain("hello world"));
/// }
/// ```
///
/// # Notes
/// This relies on the internal implementation details of the `tracing_test` crate.
#[cfg(test)]
pub(crate) mod tracing_test {
    use tracing_core::dispatcher::DefaultGuard;

    /// Create and return a `tracing` subscriber to be used in tests.
    pub(crate) fn dispatcher_guard() -> DefaultGuard {
        let mock_writer =
            ::tracing_test::internal::MockWriter::new(::tracing_test::internal::global_buf());
        let subscriber =
            ::tracing_test::internal::get_subscriber(mock_writer, "apollo_router=trace");
        tracing::dispatcher::set_default(&subscriber)
    }

    pub(crate) fn logs_with_scope_contain(scope: &str, value: &str) -> bool {
        ::tracing_test::internal::logs_with_scope_contain(scope, value)
    }

    pub(crate) fn logs_contain(value: &str) -> bool {
        logs_with_scope_contain("apollo_router", value)
    }

    pub(crate) fn logs_with_scope_assert<F>(scope: &str, f: F) -> Result<(), String>
    where
        F: Fn(&[&str]) -> Result<(), String>,
    {
        ::tracing_test::internal::logs_assert(scope, f)
    }

    pub(crate) fn logs_assert<F>(f: F) -> Result<(), String>
    where
        F: Fn(&[&str]) -> Result<(), String>,
    {
        logs_with_scope_assert("apollo_router", f)
    }
}
