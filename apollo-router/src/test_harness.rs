use std::sync::Arc;

use tower::util::BoxCloneService;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceExt;

use crate::configuration::Configuration;
use crate::plugin::test::canned;
use crate::plugin::DynPlugin;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::router_factory::RouterServiceConfigurator;
use crate::router_factory::YamlRouterServiceFactory;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
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
/// use apollo_router::services::RouterRequest;
/// use apollo_router::TestHarness;
/// use tower::util::ServiceExt;
///
/// # #[tokio::main] async fn main() -> Result<(), tower::BoxError> {
/// let config = serde_json::json!({"server": {"introspection": false}});
/// let request = RouterRequest::fake_builder()
///     // Request building here
///     .build()
///     .unwrap();
/// let response = TestHarness::builder()
///     .configuration(serde_json::from_value(config)?)
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

    /// Adds an extra, already instanciated plugin.
    ///
    /// May be called multiple times.
    /// These extra plugins are added after plugins specified in configuration.
    pub fn extra_plugin(mut self, plugin: impl Plugin) -> Self {
        fn type_name_of<T>(_: &T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = format!(
            "extra_plugins.{}.{}",
            self.extra_plugins.len(),
            type_name_of(&plugin)
        );
        self.extra_plugins.push((name, Box::new(plugin)));
        self
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
    pub async fn build(
        self,
    ) -> Result<BoxCloneService<RouterRequest, RouterResponse, BoxError>, BoxError> {
        let builder = if self.schema.is_none() {
            self.extra_plugin(CannedSubgraphResponses)
        } else {
            self
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

struct CannedSubgraphResponses;

#[async_trait::async_trait]
impl Plugin for CannedSubgraphResponses {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self)
    }

    fn subgraph_service(
        &self,
        subgraph_name: &str,
        default: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        match subgraph_name {
            "products" => canned::products_subgraph().boxed(),
            "accounts" => canned::accounts_subgraph().boxed(),
            "reviews" => canned::reviews_subgraph().boxed(),
            _ => default,
        }
    }
}
