//! Test harness and mocks for the Apollo Router.

use std::collections::HashMap;
use std::default::Default;
use std::str::FromStr;
use std::sync::Arc;

use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_http::trace::MakeSpan;
use tracing_futures::Instrument;

use crate::axum_factory::span_mode;
use crate::axum_factory::utils::PropagatingMakeSpan;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::graphql;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::PluginUnstable;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::canned;
use crate::plugins::telemetry::reload::init_telemetry;
use crate::router_factory::YamlRouterFactory;
use crate::services::HasSchema;
use crate::services::SupergraphCreator;
use crate::services::execution;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::router;
use crate::services::router::service::RouterCreator;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::spec::Schema;
#[cfg(test)]
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
        let configuration: Configuration = serde_json::from_value(configuration)?;
        Ok(self.configuration(Arc::new(configuration)))
    }

    /// Specifies the (static) router configuration as a YAML string,
    /// such as from the `serde_json::json!` macro.
    pub fn configuration_yaml(self, configuration: &'a str) -> Result<Self, ConfigurationError> {
        let configuration: Configuration = Configuration::from_str(configuration)?;
        Ok(self.configuration(Arc::new(configuration)))
    }

    /// Adds an extra, already instanciated plugin.
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
        callback: impl Fn(router::BoxService) -> router::BoxService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(RouterServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::supergraph_service`]
    pub fn supergraph_hook(
        self,
        callback: impl Fn(supergraph::BoxService) -> supergraph::BoxService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(SupergraphServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::execution_service`]
    pub fn execution_hook(
        self,
        callback: impl Fn(execution::BoxService) -> execution::BoxService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(ExecutionServicePlugin(callback))
    }

    /// Adds a callback-based hook similar to [`Plugin::subgraph_service`]
    pub fn subgraph_hook(
        self,
        callback: impl Fn(&str, subgraph::BoxService) -> subgraph::BoxService + Send + Sync + 'static,
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
    ) -> Result<(Arc<Configuration>, SupergraphCreator), BoxError> {
        let builder = if self.schema.is_none() {
            self.subgraph_hook(|subgraph_name, default| match subgraph_name {
                "products" => canned::products_subgraph().boxed(),
                "accounts" => canned::accounts_subgraph().boxed(),
                "reviews" => canned::reviews_subgraph().boxed(),
                _ => default,
            })
        } else {
            self
        };
        let builder = if builder.subgraph_network_requests {
            builder
        } else {
            builder.subgraph_hook(|name, _default| {
                let my_name = name.to_string();
                tower::service_fn(move |request: subgraph::Request| {
                    let empty_response = subgraph::Response::builder()
                        .extensions(crate::json_ext::Object::new())
                        .context(request.context)
                        .subgraph_name(my_name.clone())
                        .id(request.id)
                        .build();
                    std::future::ready(Ok(empty_response))
                })
                .boxed()
            })
        };
        let config = builder.configuration.unwrap_or_default();
        let canned_schema = include_str!("../testing_schema.graphql");
        let schema = builder.schema.unwrap_or(canned_schema);
        let schema = Arc::new(Schema::parse(schema, &config)?);
        let supergraph_creator = YamlRouterFactory
            .inner_create_supergraph(
                config.clone(),
                schema,
                None,
                None,
                Some(builder.extra_plugins),
                Default::default(),
            )
            .await?;

        Ok((config, supergraph_creator))
    }

    /// Builds the supergraph service
    pub async fn build_supergraph(self) -> Result<supergraph::BoxCloneService, BoxError> {
        let (_config, supergraph_creator) = self.build_common().await?;

        Ok(tower::service_fn(move |request| {
            let router = supergraph_creator.make();

            async move { router.oneshot(request).await }
        })
        .boxed_clone())
    }

    /// Builds the router service
    pub async fn build_router(self) -> Result<router::BoxCloneService, BoxError> {
        let (config, supergraph_creator) = self.build_common().await?;
        let router_creator = RouterCreator::new(
            QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&config)).await,
            Arc::new(PersistedQueryLayer::new(&config).await.unwrap()),
            Arc::new(supergraph_creator),
            config.clone(),
        )
        .await
        .unwrap();

        Ok(tower::service_fn(move |request: router::Request| {
            let router = ServiceBuilder::new().service(router_creator.make()).boxed();
            let span = PropagatingMakeSpan {
                license: Default::default(),
                span_mode: span_mode(&config),
            }
            .make_span(&request.router_request);
            async move { router.oneshot(request).await }.instrument(span)
        })
        .boxed_clone())
    }

    #[cfg(test)]
    pub(crate) async fn build_http_service(self) -> Result<HttpService, BoxError> {
        use crate::axum_factory::ListenAddrAndRouter;
        use crate::axum_factory::tests::make_axum_router;
        use crate::router_factory::RouterFactory;

        let (config, supergraph_creator) = self.build_common().await?;
        let router_creator = RouterCreator::new(
            QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&config)).await,
            Arc::new(PersistedQueryLayer::new(&config).await.unwrap()),
            Arc::new(supergraph_creator),
            config.clone(),
        )
        .await?;

        let web_endpoints = router_creator.web_endpoints();

        let routers = make_axum_router(
            router_creator,
            &config,
            web_endpoints,
            LicenseState::Unlicensed,
        )?;
        let ListenAddrAndRouter(_listener, router) = routers.main;
        Ok(router.boxed())
    }
}

/// An HTTP-level service, as would be given to Hyper’s server
#[cfg(test)]
pub(crate) type HttpService = tower::util::BoxService<
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
    F: 'static + Send + Sync + Fn(router::BoxService) -> router::BoxService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for SupergraphServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(supergraph::BoxService) -> supergraph::BoxService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for ExecutionServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(execution::BoxService) -> execution::BoxService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        (self.0)(service)
    }
}

#[async_trait::async_trait]
impl<F> Plugin for SubgraphServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(&str, subgraph::BoxService) -> subgraph::BoxService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        (self.0)(subgraph_name, service)
    }
}

/// a list of subgraphs with pregenerated responses
#[derive(Default)]
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
        default: subgraph::BoxService,
    ) -> subgraph::BoxService {
        self.0
            .get(subgraph_name)
            .map(|service| service.clone().boxed())
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
        if let Some((from, to)) = op_from_to {
            if let Some(operation_name) = &req.operation_name {
                if operation_name == from {
                    new_req.query = req.query.clone().map(|q| q.replace(from, to));
                    new_req.operation_name = Some(to.to_string());
                }
            }
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
