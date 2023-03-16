//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use multimap::MultiMap;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::layers::content_negociation;
use super::layers::content_negociation::ACCEPTS_MULTIPART_CONTEXT_KEY;
use super::new_service::ServiceFactory;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphServiceFactory;
use super::ExecutionServiceFactory;
use super::QueryPlannerContent;
use crate::_private::TelemetryPlugin;
use crate::error::CacheResolverError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::introspection::Introspection;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::plugin::DynPlugin;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::services::supergraph;
use crate::services::ExecutionRequest;
use crate::services::ExecutionResponse;
use crate::services::QueryPlannerRequest;
use crate::services::QueryPlannerResponse;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::spec::Schema;
use crate::Configuration;
use crate::Context;
use crate::Endpoint;
use crate::ListenAddr;

pub(crate) const QUERY_PLANNING_SPAN_NAME: &str = "query_planning";

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct SupergraphService {
    execution_service_factory: ExecutionServiceFactory,
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    schema: Arc<Schema>,
}

#[buildstructor::buildstructor]
impl SupergraphService {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
        execution_service_factory: ExecutionServiceFactory,
        schema: Arc<Schema>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service_factory,
            schema,
        }
    }
}

impl Service<SupergraphRequest> for SupergraphService {
    type Response = SupergraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.query_planner_service
            .poll_ready(cx)
            .map_err(|err| err.into())
    }

    fn call(&mut self, req: SupergraphRequest) -> Self::Future {
        // Consume our cloned services and allow ownership to be transferred to the async block.
        let clone = self.query_planner_service.clone();

        let planning = std::mem::replace(&mut self.query_planner_service, clone);
        let execution = self.execution_service_factory.create();

        let schema = self.schema.clone();

        let context_cloned = req.context.clone();
        let fut =
            service_call(planning, execution, schema, req).or_else(|error: BoxError| async move {
                let errors = vec![crate::error::Error {
                    message: error.to_string(),
                    extensions: serde_json_bytes::json!({
                        "code": "INTERNAL_SERVER_ERROR",
                    })
                    .as_object()
                    .unwrap()
                    .to_owned(),
                    ..Default::default()
                }];

                Ok(SupergraphResponse::builder()
                    .errors(errors)
                    .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                    .context(context_cloned)
                    .build()
                    .expect("building a response like this should not fail"))
            });

        Box::pin(fut)
    }
}

async fn service_call<ExecutionService>(
    planning: CachingQueryPlanner<BridgeQueryPlanner>,
    execution: ExecutionService,
    schema: Arc<Schema>,
    req: SupergraphRequest,
) -> Result<SupergraphResponse, BoxError>
where
    ExecutionService:
        Service<ExecutionRequest, Response = ExecutionResponse, Error = BoxError> + Send,
{
    let context = req.context;
    let body = req.supergraph_request.body();
    let variables = body.variables.clone();
    let QueryPlannerResponse {
        content,
        context,
        errors,
    } = match plan_query(planning, body, context.clone()).await {
        Ok(resp) => resp,
        Err(err) => match err.into_graphql_errors() {
            Ok(gql_errors) => {
                return Ok(SupergraphResponse::builder()
                    .context(context)
                    .errors(gql_errors)
                    .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
                    .build()
                    .expect("this response build must not fail"));
            }
            Err(err) => return Err(err.into()),
        },
    };

    if !errors.is_empty() {
        return Ok(SupergraphResponse::builder()
            .context(context)
            .errors(errors)
            .status_code(StatusCode::BAD_REQUEST) // If it's a graphql error we return a status code 400
            .build()
            .expect("this response build must not fail"));
    }

    match content {
        Some(QueryPlannerContent::Introspection { response }) => Ok(
            SupergraphResponse::new_from_graphql_response(*response, context),
        ),
        Some(QueryPlannerContent::IntrospectionDisabled) => {
            let mut response = SupergraphResponse::new_from_graphql_response(
                graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("introspection has been disabled"))
                        .extension_code("INTROSPECTION_DISABLED")
                        .build()])
                    .build(),
                context,
            );
            *response.response.status_mut() = StatusCode::BAD_REQUEST;
            Ok(response)
        }

        Some(QueryPlannerContent::Plan { plan }) => {
            let operation_name = body.operation_name.clone();
            let is_deferred = plan.is_deferred(operation_name.as_deref(), &variables);

            let accepts_multipart: bool = context
                .get(ACCEPTS_MULTIPART_CONTEXT_KEY)
                .unwrap_or_default()
                .unwrap_or_default();

            if is_deferred && !accepts_multipart {
                let mut response = SupergraphResponse::new_from_graphql_response(graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed; deferSpec=20220824'"))
                        .extension_code("DEFER_BAD_HEADER")
                        .build()])
                    .build(), context);
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                Ok(response)
            } else if let Some(err) = plan.query.validate_variables(body, &schema).err() {
                let mut res = SupergraphResponse::new_from_graphql_response(err, context);
                *res.response.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            } else {
                let execution_response = execution
                    .oneshot(
                        ExecutionRequest::internal_builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .schema(&schema)
                            .context(context)
                            .build()
                            .await,
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                Ok(SupergraphResponse {
                    context,
                    response: http::Response::from_parts(parts, response_stream.boxed()),
                })
            }
        }
        // This should never happen because if we have an empty query plan we should have error in errors vec
        None => Err(BoxError::from("cannot compute a query plan")),
    }
}

async fn plan_query(
    mut planning: CachingQueryPlanner<BridgeQueryPlanner>,
    body: &graphql::Request,
    context: Context,
) -> Result<QueryPlannerResponse, CacheResolverError> {
    planning
        .call(
            QueryPlannerRequest::builder()
                .query(
                    body.query
                        .clone()
                        .expect("the query presence was already checked by a plugin"),
                )
                .and_operation_name(body.operation_name.clone())
                .context(context)
                .build(),
        )
        .instrument(tracing::info_span!(
            QUERY_PLANNING_SPAN_NAME,
            graphql.document = body
                .query
                .clone()
                .expect("the query presence was already checked by a plugin")
                .as_str(),
            graphql.operation.name = body.operation_name.clone().unwrap_or_default().as_str(),
            "otel.kind" = "INTERNAL"
        ))
        .await
}

/// Builder which generates a plugin pipeline.
///
/// This is at the heart of the delegation of responsibility model for the router. A schema,
/// collection of plugins, collection of subgraph services are assembled to generate a
/// [`tower::util::BoxCloneService`] capable of processing a router request
/// through the entire stack to return a response.
pub(crate) struct PluggableSupergraphServiceBuilder {
    schema: Arc<Schema>,
    plugins: Plugins,
    subgraph_services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
    configuration: Option<Arc<Configuration>>,
}

impl PluggableSupergraphServiceBuilder {
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
    ) -> PluggableSupergraphServiceBuilder {
        self.plugins.insert(plugin_name, plugin);
        self
    }

    pub(crate) fn with_subgraph_service<S>(
        mut self,
        name: &str,
        service_maker: S,
    ) -> PluggableSupergraphServiceBuilder
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
    ) -> PluggableSupergraphServiceBuilder {
        self.configuration = Some(configuration);
        self
    }

    pub(crate) async fn build(self) -> Result<SupergraphCreator, crate::error::ServiceBuildError> {
        let configuration = self.configuration.unwrap_or_default();

        let introspection = if configuration.supergraph.introspection {
            Some(Arc::new(Introspection::new(&configuration).await))
        } else {
            None
        };

        // QueryPlannerService takes an UnplannedRequest and outputs PlannedRequest
        let bridge_query_planner =
            BridgeQueryPlanner::new(self.schema.clone(), introspection, configuration.clone())
                .await
                .map_err(ServiceBuildError::QueryPlannerError)?;
        let query_planner_service = CachingQueryPlanner::new(
            bridge_query_planner,
            self.schema.schema_id.clone(),
            &configuration.supergraph.query_planning,
        )
        .await;

        let mut plugins = self.plugins;
        // Activate the telemetry plugin.
        // We must NOT fail to go live with the new router from this point as the telemetry plugin activate interacts with globals.
        for (_, plugin) in plugins.iter_mut() {
            if let Some(telemetry) = plugin.as_any_mut().downcast_mut::<TelemetryPlugin>() {
                telemetry.activate();
            }
        }

        let plugins = Arc::new(plugins);

        let subgraph_service_factory = Arc::new(SubgraphServiceFactory::new(
            self.subgraph_services,
            plugins.clone(),
        ));

        Ok(SupergraphCreator {
            query_planner_service,
            subgraph_service_factory,
            schema: self.schema,
            plugins,
        })
    }
}

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the HTTP server to generate a new
/// RouterService on each request
pub(crate) trait SupergraphFactory:
    ServiceFactory<supergraph::Request, Service = Self::SupergraphService>
    + Clone
    + Send
    + Sync
    + 'static
{
    type SupergraphService: Service<
            supergraph::Request,
            Response = supergraph::Response,
            Error = BoxError,
            Future = Self::Future,
        > + Send;
    type Future: Send;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct SupergraphCreator {
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    subgraph_service_factory: Arc<SubgraphServiceFactory>,
    schema: Arc<Schema>,
    plugins: Arc<Plugins>,
}

pub(crate) trait HasPlugins {
    fn plugins(&self) -> Arc<Plugins>;
}

impl HasPlugins for SupergraphCreator {
    fn plugins(&self) -> Arc<Plugins> {
        self.plugins.clone()
    }
}

impl ServiceFactory<supergraph::Request> for SupergraphCreator {
    type Service = supergraph::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl SupergraphCreator {
    pub(crate) fn make(
        &self,
    ) -> impl Service<
        supergraph::Request,
        Response = supergraph::Response,
        Error = BoxError,
        Future = BoxFuture<'static, supergraph::ServiceResult>,
    > + Send {
        let supergraph_service = SupergraphService::builder()
            .query_planner_service(self.query_planner_service.clone())
            .execution_service_factory(ExecutionServiceFactory {
                schema: self.schema.clone(),
                plugins: self.plugins.clone(),
                subgraph_service_factory: self.subgraph_service_factory.clone(),
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

        ServiceBuilder::new()
            .layer(content_negociation::SupergraphLayer::default())
            .service(
                self.plugins
                    .iter()
                    .rev()
                    .fold(supergraph_service.boxed(), |acc, (_, e)| {
                        e.supergraph_service(acc)
                    }),
            )
    }

    pub(crate) async fn cache_keys(&self, count: usize) -> Vec<(String, Option<String>)> {
        self.query_planner_service.cache_keys(count).await
    }
    pub(crate) async fn warm_up_query_planner(
        &mut self,
        cache_keys: Vec<(String, Option<String>)>,
    ) {
        self.query_planner_service.warm_up(cache_keys).await
    }

    /// Create a test service.
    #[cfg(test)]
    pub(crate) async fn for_tests(
        supergraph_service: MockSupergraphService,
    ) -> MockSupergraphCreator {
        MockSupergraphCreator::new(supergraph_service).await
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MockSupergraphCreator {
    supergraph_service: MockSupergraphService,
    plugins: Arc<Plugins>,
}

#[cfg(test)]
impl MockSupergraphCreator {
    pub(crate) async fn new(supergraph_service: MockSupergraphService) -> Self {
        let canned_schema = include_str!("../../testing_schema.graphql");
        let configuration = Configuration::builder().build().unwrap();

        use crate::router_factory::create_plugins;
        let plugins = create_plugins(
            &configuration,
            &Schema::parse(canned_schema, &configuration).unwrap(),
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .collect();

        Self {
            supergraph_service,
            plugins: Arc::new(plugins),
        }
    }
}

#[cfg(test)]
impl HasPlugins for MockSupergraphCreator {
    fn plugins(&self) -> Arc<Plugins> {
        self.plugins.clone()
    }
}

#[cfg(test)]
impl ServiceFactory<supergraph::Request> for MockSupergraphCreator {
    type Service = supergraph::BoxService;
    fn create(&self) -> Self::Service {
        self.supergraph_service.clone().boxed()
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
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query("query { currentUser { activeOrganization { id creatorUser { name } } } }")
            .context(defer_context())
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
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
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
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query("query { currentUser { id  ...@defer { name } } }")
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    #[tokio::test]
    async fn errors_from_primary_on_deferred_responses() {
        let schema = r#"
        schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
        {
          query: Query
        }
        
        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
                
        scalar link__Import
        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        type Computer
          @join__type(graph: COMPUTERS)
        {
          id: ID!
          errorField: String
          nonNullErrorField: String!
        }
        
        scalar join__FieldSet
        
        enum join__Graph {
          COMPUTERS @join__graph(name: "computers", url: "http://localhost:4001/")
        }


        type Query
          @join__type(graph: COMPUTERS)
        {
          computer(id: ID!): Computer
        }"#;

        let subgraphs = MockedSubgraphs([
        ("computers", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{__typename id}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0" }}}}
            )
            .with_json(
                serde_json::json!{{
                    "query":"{computer(id:\"Computer1\"){id errorField}}",
                }},
                serde_json::json!{{
                    "data": {
                        "computer": {
                            "id": "Computer1"
                        }
                    },
                    "errors": [
                        {
                            "message": "Error field",
                            "locations": [
                                {
                                    "line": 1,
                                    "column": 93
                                }
                            ],
                            "path": ["computer","errorField"],
                        }
                    ]
                    }}
            ).build()),
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                r#"query { 
                computer(id: "Computer1") {   
                  id
                  ...ComputerErrorField @defer
                }
              }
              fragment ComputerErrorField on Computer {
                errorField
              }"#,
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    #[tokio::test]
    async fn deferred_fragment_bounds_nullability() {
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
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
        .context(defer_context())
        .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { nonNullId } } } } }",
            )
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
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
        .context(defer_context())
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
    async fn root_typename_with_defer() {
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
                }
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { __typename currentUser { activeOrganization { id  suborga { id ...@defer { name } } } } }",
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let res = stream.next_response().await.unwrap();
        assert_eq!(
            res.data.as_ref().unwrap().get("__typename"),
            Some(&serde_json_bytes::Value::String("Query".into()))
        );
        insta::assert_json_snapshot!(res);

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    #[tokio::test]
    async fn root_typename_with_defer_and_empty_first_response() {
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
                }
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { __typename ... @defer { currentUser { activeOrganization { id  suborga { id name } } } } }",
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let res = stream.next_response().await.unwrap();
        assert_eq!(
            res.data.as_ref().unwrap().get("__typename"),
            Some(&serde_json_bytes::Value::String("Query".into()))
        );

        // Must have 2 chunks
        let _ = stream.next_response().await.unwrap();
    }

    #[tokio::test]
    async fn root_typename_with_defer_in_defer() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id name}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                "query { ...@defer { __typename currentUser { activeOrganization { id  suborga { id name } } } } }",
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let _res = stream.next_response().await.unwrap();
        let res = stream.next_response().await.unwrap();
        assert_eq!(
            res.incremental
                .get(0)
                .unwrap()
                .data
                .as_ref()
                .unwrap()
                .get("__typename"),
            Some(&serde_json_bytes::Value::String("Query".into()))
        );
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

        // this test does not need to generate a valid response, it is only here to check
        // that the router does not panic when reconstructing the query for the deferred part
        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
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

    // if a deferred response falls under a path that was nullified in the primary response,
    // the deferred response must not be sent
    #[tokio::test]
    async fn filter_nullified_deferred_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{"query":"{currentUser{__typename name id}}"}},
            serde_json::json!{{"data": {"currentUser": { "__typename": "User", "name": "Ada", "id": "1" }}}}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{activeOrganization{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "User", "id":"1"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "activeOrganization": {
                                "__typename": "Organization", "id": "2"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                        "variables": {
                            "representations":[{"__typename": "User", "id":"3"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "name": "A"
                                }
                            ]
                        }
                        }})
       .build()),
        ("orga", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{creatorUser{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"2"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "creatorUser": {
                                "__typename": "User", "id": "3"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{nonNullId}}}",
                        "variables": {
                            "representations":[{"__typename": "Organization", "id":"2"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "nonNullId": null
                                }
                            ]
                        }
                        }}).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(
                r#"query {
                currentUser {
                    name
                    ... @defer {
                        activeOrganization {
                            id
                            nonNullId
                            ... @defer {
                                creatorUser {
                                    name
                                }
                            }
                        }
                    }
                }
            }"#,
            )
            .context(defer_context())
            .build()
            .unwrap();
        let mut response = service.oneshot(request).await.unwrap();

        let primary = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(primary);

        let deferred = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(deferred);

        // the last deferred response was replace with an empty response,
        // to still have one containing has_next = false
        let last = response.next_response().await.unwrap();
        insta::assert_json_snapshot!(last);
    }

    #[tokio::test]
    async fn reconstruct_deferred_query_under_interface() {
        let schema = r#"schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/tag/v0.2")
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2")
            {
                query: Query
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
                USER @join__graph(name: "user", url: "http://localhost:4000/graphql")
            }
            scalar link__Import
            enum link__Purpose {
                SECURITY
                EXECUTION
            }
            type Query
            @join__type(graph: USER)
            {
            me: Identity @join__field(graph: USER)
            }
            interface Identity
            @join__type(graph: USER)
            {
            id: ID!
            name: String!
            }
        
            type User implements Identity
                @join__implements(graph: USER, interface: "Identity")
                @join__type(graph: USER, key: "id")
            {
                fullName: String! @join__field(graph: USER)
                id: ID!
                memberships: [UserMembership!]!  @join__field(graph: USER)
                name: String! @join__field(graph: USER)
            }
            type UserMembership
                @join__type(graph: USER)
                @tag(name: "platform-api")
            {
                """The organization that the user belongs to."""
                account: Account!
                """The user's permission level within the organization."""
                permission: UserPermission!
            }
            enum UserPermission
            @join__type(graph: USER)
            {
                USER
                ADMIN
            }
            type Account
            @join__type(graph: USER, key: "id")
            {
                id: ID! @join__field(graph: USER)
                name: String!  @join__field(graph: USER)
            }"#;

        let subgraphs = MockedSubgraphs([
            ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{"query":"{me{__typename ...on User{id fullName memberships{permission account{__typename id}}}}}"}},
            serde_json::json!{{"data": {"me": {
                "__typename": "User",
                "id": 0,
                "fullName": "A",
                "memberships": [
                    {
                        "permission": "USER",
                        "account": {
                            "__typename": "Account",
                            "id": 1
                        }
                    }
                ]
            }}}}
        ) .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Account{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Account", "id": 1}
                    ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        { "__typename": "Account", "id": 1, "name": "B"}
                    ]
                }
            }}).build()),
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .context(defer_context())
            .query(
                r#"query {
                    me {
                      ... on User {
                        id
                        fullName
                        memberships {
                          permission
                          account {
                            ... on Account @defer {
                              name
                            }
                          }
                        }
                      }
                    }
                  }"#,
            )
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();

        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
        insta::assert_json_snapshot!(stream.next_response().await.unwrap());
    }

    fn defer_context() -> Context {
        let context = Context::new();
        context.insert(ACCEPTS_MULTIPART_CONTEXT_KEY, true).unwrap();
        context
    }

    #[tokio::test]
    async fn interface_object_typename_rewrites() {
        let schema = r#"
            schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
              query: Query
            }

            directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
            directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            type A implements I
              @join__implements(graph: S1, interface: "I")
              @join__type(graph: S1, key: "id")
            {
              id: ID!
              x: Int
              z: Int
              y: Int @join__field
            }

            type B implements I
              @join__implements(graph: S1, interface: "I")
              @join__type(graph: S1, key: "id")
            {
              id: ID!
              x: Int
              w: Int
              y: Int @join__field
            }

            interface I
              @join__type(graph: S1, key: "id")
              @join__type(graph: S2, key: "id", isInterfaceObject: true)
            {
              id: ID!
              x: Int @join__field(graph: S1)
              y: Int @join__field(graph: S2)
            }

            scalar join__FieldSet

            enum join__Graph {
              S1 @join__graph(name: "S1", url: "s1")
              S2 @join__graph(name: "S2", url: "s2")
            }

            scalar link__Import

            enum link__Purpose {
              SECURITY
              EXECUTION
            }

            type Query
              @join__type(graph: S1)
              @join__type(graph: S2)
            {
              iFromS1: I @join__field(graph: S1)
              iFromS2: I @join__field(graph: S2)
            }
        "#;

        let query = r#"
          {
            iFromS1 {
              ... on A {
                y
              }
            }
          }
        "#;

        let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{iFromS1{__typename ...on A{__typename id}}}",
                    }},
                    serde_json::json! {{
                        "data": {"iFromS1":{"__typename":"A","id":"idA"}}
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                // Note that this query below will only match if the input rewrite in the query plan is handled
                // correctly. Otherwise, the `representations` in the variables will have `__typename = A`
                // instead of `__typename = I`.
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on I{y}}}",
                        "variables":{"representations":[{"__typename":"I","id":"idA"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"y":42}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let response = stream.next_response().await.unwrap();

        assert_eq!(
            serde_json::to_value(&response.data).unwrap(),
            serde_json::json!({ "iFromS1": { "y": 42 } }),
        );
    }

    #[tokio::test]
    async fn interface_object_response_processing() {
        let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type Book implements Product
            @join__implements(graph: PRODUCTS, interface: "Product")
            @join__type(graph: PRODUCTS, key: "id")
          {
            id: ID!
            description: String
            price: Float
            pages: Int
            reviews: [Review!]! @join__field
          }

          scalar join__FieldSet

          enum join__Graph {
            PRODUCTS @join__graph(name: "products", url: "products")
            REVIEWS @join__graph(name: "reviews", url: "reviews")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Movie implements Product
            @join__implements(graph: PRODUCTS, interface: "Product")
            @join__type(graph: PRODUCTS, key: "id")
          {
            id: ID!
            description: String
            price: Float
            duration: Int
            reviews: [Review!]! @join__field
          }

          interface Product
            @join__type(graph: PRODUCTS, key: "id")
            @join__type(graph: REVIEWS, key: "id", isInterfaceObject: true)
          {
            id: ID!
            description: String @join__field(graph: PRODUCTS)
            price: Float @join__field(graph: PRODUCTS)
            reviews: [Review!]! @join__field(graph: REVIEWS)
          }

          type Query
            @join__type(graph: PRODUCTS)
            @join__type(graph: REVIEWS)
          {
            products: [Product!]! @join__field(graph: PRODUCTS)
            allReviewedProducts: [Product!]! @join__field(graph: REVIEWS)
            bestRatedProducts(limit: Int): [Product!]! @join__field(graph: REVIEWS)
          }

          type Review
            @join__type(graph: REVIEWS)
          {
            author: String
            text: String
            rating: Int
          }
        "#;

        let query = r#"
          {
            allReviewedProducts {
              id
              price
            }
          }
        "#;

        let subgraphs = MockedSubgraphs([
            ("products", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{__typename price}}}",
                        "variables": {"representations":[{"__typename":"Product","id":"1"},{"__typename":"Product","id":"2"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"price":12.99},{"price":14.99}]}
                    }},
                )
                .build()),
            ("reviews", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{allReviewedProducts{__typename id}}"
                    }},
                    serde_json::json! {{
                        "data": {"allReviewedProducts":[{"__typename":"Product","id":"1"},{"__typename":"Product","id":"2"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let response = stream.next_response().await.unwrap();
        println!("{response:?}");

        assert_eq!(
            serde_json::to_value(&response.data).unwrap(),
            serde_json::json!({ "allReviewedProducts": [ {"id": "1", "price": 12.99}, {"id": "2", "price": 14.99} ]}),
        );
    }

    #[tokio::test]
    async fn only_query_interface_object_subgraph() {
        // This test has 2 subgraphs, one with an interface and another with that interface
        // declared as an @interfaceObject. It then sends a query that can be entirely
        // fulfilled by the @interfaceObject subgraph (in particular, it doesn't request
        // __typename; if it did, it would force a query on the other subgraph to obtain
        // the actual implementation type).
        // The specificity here is that the final in-memory result will not have a __typename
        // _despite_ being the parent type of that result being an interface. Which is fine
        // since __typename is not requested, and so there is no need to known the actual
        // __typename, but this is something that never happen outside of @interfaceObject
        // (usually, results whose parent type is an abstract type (say an interface) are always
        // queried internally with their __typename). And so this test make sure that the
        // post-processing done by the router on the result handle this correctly.

        let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

          directive @join__graph(name: String!, url: String!) on ENUM_VALUE

          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type A implements I
            @join__implements(graph: S1, interface: "I")
            @join__type(graph: S1, key: "id")
          {
            id: ID!
            x: Int
            z: Int
            y: Int @join__field
          }

          type B implements I
            @join__implements(graph: S1, interface: "I")
            @join__type(graph: S1, key: "id")
          {
            id: ID!
            x: Int
            w: Int
            y: Int @join__field
          }

          interface I
            @join__type(graph: S1, key: "id")
            @join__type(graph: S2, key: "id", isInterfaceObject: true)
          {
            id: ID!
            x: Int @join__field(graph: S1)
            y: Int @join__field(graph: S2)
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "S1")
            S2 @join__graph(name: "S2", url: "S2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            iFromS1: I @join__field(graph: S1)
            iFromS2: I @join__field(graph: S2)
          }
        "#;

        let query = r#"
          {
            iFromS2 {
              y
            }
          }
        "#;

        let subgraphs = MockedSubgraphs(
            [
                (
                    "S1",
                    MockSubgraph::builder()
                        // This test makes no queries to S1, only to S2
                        .build(),
                ),
                (
                    "S2",
                    MockSubgraph::builder()
                        .with_json(
                            serde_json::json! {{
                                "query": "{iFromS2{y}}",
                            }},
                            serde_json::json! {{
                                "data": {"iFromS2":{"y":20}}
                            }},
                        )
                        .build(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let response = stream.next_response().await.unwrap();

        assert_eq!(
            serde_json::to_value(&response.data).unwrap(),
            serde_json::json!({ "iFromS2": { "y": 20 } }),
        );
    }

    #[tokio::test]
    async fn aliased_subgraph_data_rewrites_on_root_fetch() {
        let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

          type A implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: String
          }

          type B implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: Int
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "s1")
            S2 @join__graph(name: "S2", url: "s2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            us: [U] @join__field(graph: S1)
          }

          interface U
            @join__type(graph: S1)
          {
            f: String
          }
        "#;

        let query = r#"
          {
            us {
              f
            }
          }
        "#;

        let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{us{__typename ...on A{__typename g}...on B{__typename g__alias_0:g}}}",
                    }},
                    serde_json::json! {{
                        "data": {"us":[{"__typename":"A","g":"foo"},{"__typename":"B","g__alias_0":1}]},
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                .with_json(
                    // Note that the query below will only match if the output rewrite in the query plan is handled
                    // correctly. Otherwise, the `representations` in the variables will not be able to find the
                    // field `g` for the `B` object, since it was returned as `g__alias_0` on the initial subgraph
                    // query above.
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on A{f}...on B{f}}}",
                        "variables":{"representations":[{"__typename":"A","g":"foo"},{"__typename":"B","g":1}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"f":"fA"},{"f":"fB"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let response = stream.next_response().await.unwrap();

        assert_eq!(
            serde_json::to_value(&response.data).unwrap(),
            serde_json::json!({"us": [{"f": "fA"}, {"f": "fB"}]}),
        );
    }

    #[tokio::test]
    async fn aliased_subgraph_data_rewrites_on_non_root_fetch() {
        let schema = r#"
          schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
          {
            query: Query
          }

          directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
          directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
          directive @join__graph(name: String!, url: String!) on ENUM_VALUE
          directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
          directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
          directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
          directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
          type A implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: String
          }

          type B implements U
            @join__implements(graph: S1, interface: "U")
            @join__type(graph: S1, key: "g")
            @join__type(graph: S2, key: "g")
          {
            f: String @join__field(graph: S1, external: true) @join__field(graph: S2)
            g: Int
          }

          scalar join__FieldSet

          enum join__Graph {
            S1 @join__graph(name: "S1", url: "s1")
            S2 @join__graph(name: "S2", url: "s2")
          }

          scalar link__Import

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          type Query
            @join__type(graph: S1)
            @join__type(graph: S2)
          {
            t: T @join__field(graph: S2)
          }

          type T
            @join__type(graph: S1, key: "id")
            @join__type(graph: S2, key: "id")
          {
            id: ID!
            us: [U] @join__field(graph: S1)
          }

          interface U
            @join__type(graph: S1)
          {
            f: String
          }
        "#;

        let query = r#"
          {
            t {
              us {
                f
              }
            }
          }
        "#;

        let subgraphs = MockedSubgraphs([
            ("S1", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on T{us{__typename ...on A{__typename g}...on B{__typename g__alias_0:g}}}}}",
                        "variables":{"representations":[{"__typename":"T","id":"0"}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"us":[{"__typename":"A","g":"foo"},{"__typename":"B","g__alias_0":1}]}]},
                    }},
                )
                .build()),
            ("S2", MockSubgraph::builder()
                .with_json(
                    serde_json::json! {{
                        "query": "{t{__typename id}}",
                    }},
                    serde_json::json! {{
                        "data": {"t":{"__typename":"T","id":"0"}},
                    }},
                )
                // Note that this query will only match if the output rewrite in the query plan is handled correctly. Otherwise,
                // the `representations` in the variables will not be able to find the field `g` for the `B` object, since it was
                // returned as `g__alias_0` on the (non-root) S1 query above.
                .with_json(
                    serde_json::json! {{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on A{f}...on B{f}}}",
                        "variables":{"representations":[{"__typename":"A","g":"foo"},{"__typename":"B","g":1}]}
                    }},
                    serde_json::json! {{
                        "data": {"_entities":[{"f":"fA"},{"f":"fB"}]}
                    }},
                )
                .build()),
        ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .extra_plugin(subgraphs)
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();

        let mut stream = service.oneshot(request).await.unwrap();
        let response = stream.next_response().await.unwrap();

        assert_eq!(
            serde_json::to_value(&response.data).unwrap(),
            serde_json::json!({"t": {"us": [{"f": "fA"}, {"f": "fB"}]}}),
        );
    }
}
