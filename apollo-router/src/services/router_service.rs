//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use futures::stream::StreamExt;
use http::Method;
use indexmap::IndexMap;
use multimap::MultiMap;
use tower::util::BoxService;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use super::new_service::ServiceFactory;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphCreator;
use super::supergraph;
use super::ExecutionCreator;
use super::SupergraphService;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::introspection::Introspection;
use crate::plugin::DynPlugin;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::router_factory::Endpoint;
use crate::router_factory::RouterFactory;
use crate::services::layers::ensure_query_presence::EnsureQueryPresence;
use crate::Configuration;
use crate::ListenAddr;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::Schema;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
pub(crate) struct RouterService {
    supergraph_service: supergraph::BoxService,
}

impl RouterService {
    pub(crate) fn new(supergraph_service: supergraph::BoxService) -> Self {
        RouterService { supergraph_service }
    }
}

impl Service<RouterRequest> for RouterService {
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(())
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let RouterRequest {
            router_request,
            context,
        } = req;

        // let clone = self.supergraph_service.clone();
        // let supergraph_service = std::mem::replace(&mut self.supergraph_service, clone);

        let (parts, body) = router_request.into_parts();

        // TODO[igni]: deal with errors
        //     (StatusCode::BAD_REQUEST, "Invalid GraphQL request").into_response() will help
        let fut = async move {
            let graphql_request = if parts.method == Method::GET {
                parts
                    .uri
                    .query()
                    .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
                    .unwrap()
            } else {
                let bytes = hyper::body::to_bytes(body).await.unwrap();
                serde_json::from_reader(bytes.reader()).unwrap()
            };

            let request = SupergraphRequest {
                supergraph_request: http::Request::from_parts(parts, graphql_request),
                context,
            };

            // todo: factory.create et oneshot.

            let SupergraphResponse { response, context } =
                self.supergraph_service.call(request).await.unwrap();

            let (parts, body) = response.into_parts();

            Ok(RouterResponse {
                response: http::Response::from_parts(
                    parts,
                    hyper::Body::wrap_stream(body.map(|chunk| serde_json::to_vec(&chunk))),
                ),
                context,
            })
        };
        Box::pin(fut)
    }
}

/// Builder which generates a plugin pipeline.
///
/// This is at the heart of the delegation of responsibility model for the router. A schema,
/// collection of plugins, collection of subgraph services are assembled to generate a
/// [`tower::util::BoxCloneService`] capable of processing a router request
/// through the entire stack to return a response.
pub(crate) struct PluggableRouterServiceBuilder {
    schema: Arc<Schema>,
    plugins: Plugins,
    subgraph_services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
    configuration: Option<Arc<Configuration>>,
}

impl PluggableRouterServiceBuilder {
    pub(crate) fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            plugins: Default::default(),
            subgraph_services: Default::default(),
            configuration: None,
        }
    }

    pub(crate) fn with_dyn_plugin(
        mut self,
        plugin_name: String,
        plugin: Box<dyn DynPlugin>,
    ) -> PluggableRouterServiceBuilder {
        self.plugins.insert(plugin_name, plugin);
        self
    }

    pub(crate) fn with_subgraph_service<S>(
        mut self,
        name: &str,
        service_maker: S,
    ) -> PluggableRouterServiceBuilder
    where
        S: MakeSubgraphService,
    {
        self.subgraph_services
            .push((name.to_string(), Arc::new(service_maker)));
        self
    }

    pub(crate) fn with_configuration(
        mut self,
        configuration: Arc<Configuration>,
    ) -> PluggableRouterServiceBuilder {
        self.configuration = Some(configuration);
        self
    }

    pub(crate) async fn build(self) -> Result<RouterCreator, crate::error::ServiceBuildError> {
        // Note: The plugins are always applied in reverse, so that the
        // fold is applied in the correct sequence. We could reverse
        // the list of plugins, but we want them back in the original
        // order at the end of this function. Instead, we reverse the
        // various iterators that we create for folding and leave
        // the plugins in their original order.

        let configuration = self.configuration.unwrap_or_default();

        let plan_cache_limit = std::env::var("ROUTER_PLAN_CACHE_LIMIT")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(100);
        let redis_urls = configuration.supergraph.cache();

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(&configuration).await))
        } else {
            None
        };

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner =
            BridgeQueryPlanner::new(self.schema.clone(), introspection, configuration)
                .await
                .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service = CachingQueryPlanner::new(
            bridge_query_planner,
            plan_cache_limit,
            self.schema.schema_id.clone(),
            redis_urls,
        )
        .await;

        let plugins = Arc::new(self.plugins);

        let subgraph_creator = Arc::new(SubgraphCreator::new(
            self.subgraph_services,
            plugins.clone(),
        ));

        Ok(RouterCreator {
            query_planner_service,
            subgraph_creator,
            schema: self.schema,
            plugins,
        })
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    subgraph_creator: Arc<SubgraphCreator>,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
}

impl ServiceFactory<RouterRequest> for RouterCreator {
    type Service = BoxService<RouterRequest, RouterResponse, BoxError>;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl RouterFactory for RouterCreator {
    type RouterService = BoxService<RouterRequest, RouterResponse, BoxError>;

    type Future = <<RouterCreator as ServiceFactory<RouterRequest>>::Service as Service<
        RouterRequest,
    >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.plugins
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }
}

impl RouterCreator {
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        RouterRequest,
        Response = RouterResponse,
        Error = BoxError,
        Future = BoxFuture<'static, Result<RouterResponse, BoxError>>,
    > + Send {
        let supergraph_service = SupergraphService::builder()
            .query_planner_service(self.query_planner_service.clone())
            .execution_service_factory(ExecutionCreator {
                schema: self.schema.clone(),
                plugins: self.plugins.clone(),
                subgraph_creator: self.subgraph_creator.clone(),
            })
            .schema(self.schema.clone())
            .build();

        let supergraph_service = match self
            .plugins
            .iter()
            .find(|i| i.0.as_str() == APOLLO_TRAFFIC_SHAPING)
            .and_then(|plugin| plugin.1.as_any().downcast_ref::<TrafficShaping>())
        {
            Some(shaping) => Either::A(shaping.supergraph_service_internal(supergraph_service)),
            None => Either::B(supergraph_service),
        };

        let supergraph_service = ServiceBuilder::new()
            .layer(EnsureQueryPresence::default())
            .service(
                self.plugins
                    .iter()
                    .rev()
                    .fold(supergraph_service.boxed(), |acc, (_, e)| {
                        e.supergraph_service(acc)
                    }),
            );

        let router_service = RouterService::new(supergraph_service.boxed());

        ServiceBuilder::new().service(
            self.plugins
                .iter()
                .rev()
                .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
        )
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) fn test_service(
        &self,
    ) -> tower::util::BoxCloneService<RouterRequest, RouterResponse, BoxError> {
        use tower::buffer::Buffer;

        Buffer::new(self.make(), 512).boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::test::MockSubgraph;
    use crate::services::supergraph;
    use crate::test_harness::MockedSubgraphs;
    use crate::TestHarness;

    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet

   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }

   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
   }

   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

    #[tokio::test]
    async fn nullability_formatting() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": null }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query("query { currentUser { activeOrganization { id creatorUser { name } } } }")
            // Request building here
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }

    #[tokio::test]
    async fn nullability_bubbling() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {} }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(
                "query { currentUser { activeOrganization { nonNullId creatorUser { name } } } }",
            )
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        insta::assert_json_snapshot!(response);
    }

    #[tokio::test]
    async fn errors_on_deferred_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{__typename id}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0" }}}}
            )
            .with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                    "variables": {
                        "representations":[{"__typename": "User", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "User", "name": "AAA"},
                        ] }]
                    },
                    "errors": [
                        {
                            "message": "error user 0",
                            "path": ["_entities", 0],
                        }
                    ]
                    }}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query("query { currentUser { id  ...@defer { name } } }")
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    #[tokio::test]
    async fn errors_on_incremental_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                },
                "errors": [
                    {
                        "message": "error orga 1",
                        "path": ["_entities", 0],
                    },
                    {
                        "message": "error orga 3",
                        "path": ["_entities", 2],
                    }
                ]
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { name } } } } }",
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    #[tokio::test]
    async fn query_reconstruction() {
        let schema = r#"schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/tag/v0.2")
    @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
  {
    query: Query
    mutation: Mutation
  }
  
  directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
  
  directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
  
  directive @join__graph(name: String!, url: String!) on ENUM_VALUE
  
  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
  
  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
  
  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
  
  directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
  
  scalar join__FieldSet
  
  enum join__Graph {
    PRODUCTS @join__graph(name: "products", url: "http://products:4000/graphql")
    USERS @join__graph(name: "users", url: "http://users:4000/graphql")
  }
  
  scalar link__Import
  
  enum link__Purpose {
    SECURITY
    EXECUTION
  }
  
  type MakePaymentResult
    @join__type(graph: USERS)
  {
    id: ID!
    paymentStatus: PaymentStatus
  }
  
  type Mutation
    @join__type(graph: USERS)
  {
    makePayment(userId: ID!): MakePaymentResult!
  }
  
  
 type PaymentStatus
    @join__type(graph: USERS)
  {
    id: ID!
  }
  
  type Query
    @join__type(graph: PRODUCTS)
    @join__type(graph: USERS)
  {
    name: String
  }
  "#;

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .build()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query(
                r#"mutation ($userId: ID!) {
                    makePayment(userId: $userId) {
                      id
                      ... @defer {
                        paymentStatus {
                          id
                        }
                      }
                    }
                  }"#,
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }
}
