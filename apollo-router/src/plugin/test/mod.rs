//! Utilities which make it easy to test with [`crate::plugin`].

mod mock;
mod service;

use std::collections::HashMap;
use std::sync::Arc;

pub use mock::subgraph::MockSubgraph;
pub use service::MockExecutionService;
pub use service::MockQueryPlanningService;
pub use service::MockRouterService;
pub use service::MockSubgraphService;
use tower::buffer::Buffer;
use tower::util::BoxCloneService;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::router_factory::RouterServiceConfigurator;
use crate::router_factory::YamlRouterServiceFactory;
use crate::services::subgraph_service::SubgraphServiceFactory;
use crate::services::Plugins;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::Schema;

pub struct PluginTestHarness {
    router_service: BoxCloneService<RouterRequest, RouterResponse, BoxError>,
    plugins: Arc<Plugins>,
}

pub(crate) type BufferedSubgraphService = Buffer<
    BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>,
    crate::SubgraphRequest,
>;

#[buildstructor::buildstructor]
impl PluginTestHarness {
    /// Plugin test harness gives you an easy way to test your plugins against a mock subgraph.
    /// Currently mocking is basic, and only a request for topProducts is supported
    ///
    /// # Arguments
    ///
    /// * `plugin`: The plugin to test
    /// * `schema`: (Optional) the supergraph schema to use. If not provided, a canned testing schema is used.
    /// * `mock_router_service`: (Optional) router service. If none is supplied it will be defaulted.
    /// * `mock_query_planner_service`: (Optional) query planner service. If none is supplied it will be defaulted.
    /// * `mock_execution_service`: (Optional) execution service. If none is supplied it will be defaulted.
    /// * `mock_subgraph_services`: (Optional) subgraph service. If none is supplied it will be defaulted.
    ///
    /// returns: Result<PluginTestHarness, Box<dyn Error+Send+Sync, Global>>
    ///
    #[builder]
    #[allow(clippy::needless_lifetimes)] // Not needless in the builder
    pub async fn new<'schema>(
        configuration: serde_json::Value,
        schema: Option<&'schema str>,
        mock_router_service: Option<MockRouterService>,
        mock_query_planner_service: Option<MockQueryPlanningService>,
        mock_execution_service: Option<MockExecutionService>,
        mock_subgraph_services: HashMap<String, MockSubgraphService>,
    ) -> Result<PluginTestHarness, BoxError> {
        let configuration: Arc<_> = serde_json::from_value(configuration)?;
        let canned_schema = schema.is_none();
        let schema = schema.unwrap_or(include_str!("../../../../examples/graphql/local.graphql"));
        let schema = Arc::new(Schema::parse(schema, &configuration)?);

        let mut subgraph_services = mock_subgraph_services
            .into_iter()
            .map(|(k, v)| (k, Buffer::new(v.build().boxed(), DEFAULT_BUFFER_SIZE)))
            .collect::<HashMap<_, BufferedSubgraphService>>();
        // If we're using the canned schema then add some canned results
        if canned_schema {
            subgraph_services
                .entry("products".to_string())
                .or_insert_with(|| {
                    Buffer::new(
                        mock::canned::products_subgraph().boxed(),
                        DEFAULT_BUFFER_SIZE,
                    )
                });
            subgraph_services
                .entry("accounts".to_string())
                .or_insert_with(|| {
                    Buffer::new(
                        mock::canned::accounts_subgraph().boxed(),
                        DEFAULT_BUFFER_SIZE,
                    )
                });
            subgraph_services
                .entry("reviews".to_string())
                .or_insert_with(|| {
                    Buffer::new(
                        mock::canned::reviews_subgraph().boxed(),
                        DEFAULT_BUFFER_SIZE,
                    )
                });
        }

        let router_creator = YamlRouterServiceFactory
            .create_with_mocks(
                configuration,
                schema,
                mock_router_service,
                mock_query_planner_service,
                mock_execution_service,
                Some(subgraph_services),
                None,
            )
            .await?;
        let router_service = router_creator.test_service();
        Ok(Self {
            router_service,
            plugins: router_creator.plugins,
        })
    }

    /// Call the test harness with a request. Not that you will need to have set up appropriate responses via mocks.
    pub async fn call(&mut self, request: RouterRequest) -> Result<RouterResponse, BoxError> {
        self.router_service.ready().await?.call(request).await
    }

    /// If using the canned schema this canned request will give a response.
    pub async fn call_canned(&mut self) -> Result<RouterResponse, BoxError> {
        self.router_service
            .ready()
            .await?
            .call(
                RouterRequest::fake_builder()
                    .query("query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }")
                    .variable("first", 2usize)
                    .build()?,
            )
            .await
    }

    /// Return the plugin instance that has the given `name`
    ///
    /// Returns `None` if the name or `Plugin` type does not match those given to [`register_plugin!`],
    /// or if that plugin was not enabled in configuration.
    pub fn plugin<Plugin: 'static>(&self, name: &str) -> Option<&Plugin> {
        self.plugins.get(name)?.downcast_ref::<Plugin>()
    }
}

#[derive(Clone)]
pub struct MockSubgraphFactory {
    pub(crate) subgraphs: HashMap<
        String,
        Buffer<
            BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>,
            SubgraphRequest,
        >,
    >,
    pub(crate) plugins: Arc<Plugins>,
}

impl SubgraphServiceFactory for MockSubgraphFactory {
    type SubgraphService = BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>;

    type Future =
        <BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError> as Service<
            SubgraphRequest,
        >>::Future;

    fn new_service(&self, name: &str) -> Option<Self::SubgraphService> {
        self.subgraphs.get(name).map(|service| {
            self.plugins
                .iter()
                .rev()
                .fold(service.clone().boxed(), |acc, (_, e)| {
                    e.subgraph_service(name, acc)
                })
        })
    }
}

#[cfg(test)]
mod testing {
    use insta::assert_json_snapshot;
    use serde_json::json;

    use super::*;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::register_plugin;

    struct EmptyPlugin {}
    #[async_trait::async_trait]
    impl Plugin for EmptyPlugin {
        type Config = ();

        async fn new(_init: PluginInit<Self::Config>) -> Result<Self, tower::BoxError> {
            Ok(Self {})
        }
    }
    register_plugin!("apollo.test", "empty_plugin", EmptyPlugin);

    #[tokio::test]
    async fn test_test_harness() -> Result<(), BoxError> {
        let mut harness = PluginTestHarness::builder()
            .configuration(json!({ "test.empty_plugin": null }))
            .build()
            .await?;
        let graphql = harness.call_canned().await?.next_response().await.unwrap();
        assert_eq!(graphql.errors, []);
        insta::with_settings!({sort_maps => true}, {
            assert_json_snapshot!(graphql.data);
        });
        Ok(())
    }
}
