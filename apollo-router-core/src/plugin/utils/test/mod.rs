//! Utilities which make it easy to test with [`crate::plugin`].

pub mod mock;
pub mod service;

use crate::services::layers::apq::APQLayer;
use crate::services::layers::ensure_query_presence::EnsureQueryPresence;
use crate::CachingQueryPlanner;
use crate::ExecutionService;
use crate::Introspection;
use crate::Plugin;
use crate::QueryCache;
use crate::RouterService;
use crate::Schema;
use crate::{BridgeQueryPlanner, DEFAULT_BUFFER_SIZE};
use crate::{RouterRequest, RouterResponse};
pub use service::{
    MockExecutionService, MockQueryPlanningService, MockRouterService, MockSubgraphService,
};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::Service;
use tower::ServiceExt;
use tower::{BoxError, ServiceBuilder};

pub struct PluginTestHarness {
    router_service: BoxService<RouterRequest, RouterResponse, BoxError>,
}
pub enum IntoSchema {
    String(String),
    Schema(Box<Schema>),
    Canned,
}

impl From<Schema> for IntoSchema {
    fn from(schema: Schema) -> Self {
        IntoSchema::Schema(Box::new(schema))
    }
}
impl From<String> for IntoSchema {
    fn from(schema: String) -> Self {
        IntoSchema::String(schema)
    }
}

impl From<IntoSchema> for Schema {
    fn from(s: IntoSchema) -> Self {
        match s {
            IntoSchema::String(s) => Schema::from_str(&s).expect("test schema must be valid"),
            IntoSchema::Schema(s) => *s,
            IntoSchema::Canned => Schema::from_str(include_str!(
                "../../../../../examples/graphql/local.graphql"
            ))
            .expect("test schema must be valid"),
        }
    }
}

#[buildstructor::builder]
impl PluginTestHarness {
    /// Plugin test harness gives you an easy way to test your plugins against a mock subgraph.
    /// Currently mocking is basic, and only a request for topProducts is supported
    ///
    /// # Arguments
    ///
    /// * `plugin`: The plugin to test
    /// * `schema`: The schema, either Canned, or a custom schema.
    /// * `mock_router_service`: (Optional) router service. If none is supplied it will be defaulted.
    /// * `mock_query_planner_service`: (Optional) query planner service. If none is supplied it will be defaulted.
    /// * `mock_execution_service`: (Optional) execution service. If none is supplied it will be defaulted.
    /// * `mock_subgraph_services`: (Optional) subgraph service. If none is supplied it will be defaulted.
    ///
    /// returns: Result<PluginTestHarness, Box<dyn Error+Send+Sync, Global>>
    ///
    pub async fn new<P: Plugin>(
        mut plugin: P,
        schema: IntoSchema,
        mock_router_service: Option<MockRouterService>,
        mock_query_planner_service: Option<MockQueryPlanningService>,
        mock_execution_service: Option<MockExecutionService>,
        mock_subgraph_services: HashMap<String, MockSubgraphService>,
    ) -> Result<PluginTestHarness, BoxError> {
        let mut subgraph_services = mock_subgraph_services
            .into_iter()
            .map(|(k, v)| {
                let subgraph_service = plugin.subgraph_service(&k, v.build().boxed());
                (
                    k.clone(),
                    Buffer::new(subgraph_service, DEFAULT_BUFFER_SIZE),
                )
            })
            .collect::<HashMap<_, _>>();
        // If we're using the canned schema then add some canned results
        if let IntoSchema::Canned = schema {
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

        let schema = Arc::new(Schema::from(schema));

        let query_planner = CachingQueryPlanner::new(
            BridgeQueryPlanner::new(schema.clone()).await?,
            DEFAULT_BUFFER_SIZE,
        )
        .boxed();
        let query_planner_service = plugin.query_planning_service(
            mock_query_planner_service
                .map(|s| s.build().boxed())
                .unwrap_or(query_planner),
        );

        let execution_service = plugin.execution_service(
            mock_execution_service
                .map(|s| s.build().boxed())
                .unwrap_or_else(|| {
                    ExecutionService::builder()
                        .schema(schema.clone())
                        .subgraph_services(subgraph_services)
                        .build()
                        .boxed()
                }),
        );

        let router_service = ServiceBuilder::new()
            .layer(APQLayer::default())
            .layer(EnsureQueryPresence::default())
            .service(
                plugin.router_service(
                    mock_router_service
                        .map(|s| s.build().boxed())
                        .unwrap_or_else(|| {
                            BoxService::new(
                                RouterService::builder()
                                    .query_planner_service(Buffer::new(
                                        query_planner_service,
                                        DEFAULT_BUFFER_SIZE,
                                    ))
                                    .query_execution_service(Buffer::new(
                                        execution_service,
                                        DEFAULT_BUFFER_SIZE,
                                    ))
                                    .schema(schema.clone())
                                    .query_cache(Arc::new(QueryCache::new(0, schema.clone())))
                                    .introspection(Arc::new(Introspection::from_schema(&schema)))
                                    .build(),
                            )
                        }),
                ),
            )
            .boxed();
        Ok(Self { router_service })
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
}

#[cfg(test)]
mod testing {
    use super::*;
    use insta::assert_json_snapshot;

    struct EmptyPlugin {}
    #[async_trait::async_trait]
    impl Plugin for EmptyPlugin {
        type Config = ();

        async fn new(_config: Self::Config) -> Result<Self, tower::BoxError> {
            Ok(Self {})
        }
    }

    #[tokio::test]
    async fn test_test_harness() -> Result<(), BoxError> {
        let mut harness = PluginTestHarness::builder()
            .plugin(EmptyPlugin {})
            .schema(IntoSchema::Canned)
            .build()
            .await?;
        let result = harness.call_canned().await?;
        if let crate::ResponseBody::GraphQL(graphql) = result.response.body() {
            insta::with_settings!({sort_maps => true}, {
                assert_json_snapshot!(graphql.data);
            });
        } else {
            panic!("Should have got response body");
        }
        Ok(())
    }
}
