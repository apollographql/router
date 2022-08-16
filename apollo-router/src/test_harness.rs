use std::sync::Arc;

use tower::BoxError;
use tower::ServiceExt;

use crate::configuration::Configuration;
use crate::plugin::test::canned;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::router_factory::RouterServiceConfigurator;
use crate::router_factory::YamlRouterServiceFactory;
use crate::stages::execution;
use crate::stages::query_planner;
use crate::stages::router;
use crate::stages::subgraph;
use crate::Schema;

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
/// use apollo_router::stages::router;
/// use apollo_router::TestHarness;
/// use tower::util::ServiceExt;
///
/// # #[tokio::main] async fn main() -> Result<(), tower::BoxError> {
/// let config = serde_json::json!({"server": {"introspection": false}});
/// let request = router::Request::fake_builder()
///     // Request building here
///     .build()
///     .unwrap();
/// let response = TestHarness::builder()
///     .configuration_json(config)?
///     .build()
///     .await?
///     .oneshot(request)
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
    pub fn builder() -> Self {
        Self {
            schema: None,
            configuration: None,
            extra_plugins: Vec::new(),
            subgraph_network_requests: false,
        }
    }

    /// Specifies the (static) supergraph schema definition.
    ///
    /// Panics if called more than once.
    ///
    /// If this isn’t called, a default “canned” schema is used.
    /// It can be found in the Router repository at `examples/graphql/local.graphql`.
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
        Ok(self.configuration(serde_json::from_value(configuration)?))
    }

    /// Adds an extra, already instanciated plugin.
    ///
    /// May be called multiple times.
    /// These extra plugins are added after plugins specified in configuration.
    pub fn extra_plugin<P: Plugin>(mut self, plugin: P) -> Self {
        let type_id = std::any::TypeId::of::<P>();
        let name = match crate::plugin::plugins()
            .iter()
            .find(|(_name, factory)| factory.type_id == type_id)
        {
            Some((name, _factory)) => name.clone(),
            None => format!(
                "extra_plugins.{}.{}",
                self.extra_plugins.len(),
                std::any::type_name::<P>(),
            ),
        };

        self.extra_plugins.push((name, Box::new(plugin)));
        self
    }

    /// Adds an ad-hoc plugin that has [`Plugin::router_service`] implemented with `callback`.
    pub fn extra_router_plugin(
        self,
        callback: impl Fn(router::BoxService) -> router::BoxService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(RouterServicePlugin(callback))
    }

    /// Adds an ad-hoc plugin that has [`Plugin::query_planning_service`] implemented with `callback`.
    pub fn extra_query_planner_plugin(
        self,
        callback: impl Fn(query_planner::BoxService) -> query_planner::BoxService
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.extra_plugin(QueryPlannerServicePlugin(callback))
    }

    /// Adds an ad-hoc plugin that has [`Plugin::execution_service`] implemented with `callback`.
    pub fn extra_execution_plugin(
        self,
        callback: impl Fn(execution::BoxService) -> execution::BoxService + Send + Sync + 'static,
    ) -> Self {
        self.extra_plugin(ExecutionServicePlugin(callback))
    }

    /// Adds an ad-hoc plugin that has [`Plugin::subgraph_service`] implemented with `callback`.
    pub fn extra_subgraph_plugin(
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

    /// Builds the GraphQL service
    pub async fn build(self) -> Result<router::BoxCloneService, BoxError> {
        let builder = if self.schema.is_none() {
            self.extra_subgraph_plugin(|subgraph_name, default| match subgraph_name {
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
            builder.extra_subgraph_plugin(|_name, _default| {
                tower::service_fn(|request: subgraph::Request| {
                    let empty_response = subgraph::Response::builder()
                        .extensions(crate::json_ext::Object::new())
                        .context(request.context)
                        .build();
                    std::future::ready(Ok(empty_response))
                })
                .boxed()
            })
        };
        let config = builder.configuration.unwrap_or_default();
        let canned_schema = include_str!("../../examples/graphql/local.graphql");
        let schema = builder.schema.unwrap_or(canned_schema);
        let schema = Arc::new(Schema::parse(schema, &config)?);
        let router_creator = YamlRouterServiceFactory
            .create(config, schema, None, Some(builder.extra_plugins))
            .await?;
        Ok(tower::service_fn(move |request| {
            let service = router_creator.make();
            async move { service.oneshot(request).await }
        })
        .boxed_clone())
    }
}

struct RouterServicePlugin<F>(F);
struct QueryPlannerServicePlugin<F>(F);
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
impl<F> Plugin for QueryPlannerServicePlugin<F>
where
    F: 'static + Send + Sync + Fn(query_planner::BoxService) -> query_planner::BoxService,
{
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!()
    }

    fn query_planning_service(
        &self,
        service: query_planner::BoxService,
    ) -> query_planner::BoxService {
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
