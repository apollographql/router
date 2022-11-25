//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use futures::future::BoxFuture;
use futures::stream::StreamExt;
use futures::TryFutureExt;
use http::StatusCode;
use indexmap::IndexMap;
use multimap::MultiMap;
use opentelemetry::trace::SpanKind;
use tower::util::BoxService;
use tower::util::Either;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;
use tracing_futures::Instrument;

use super::new_service::NewService;
use super::subgraph_service::MakeSubgraphService;
use super::subgraph_service::SubgraphCreator;
use super::ExecutionCreator;
use super::ExecutionServiceFactory;
use super::QueryPlannerContent;
use crate::axum_factory::utils::accepts_multipart;
use crate::error::CacheResolverError;
use crate::error::ServiceBuildError;
use crate::graphql;
use crate::graphql::IntoGraphQLErrors;
use crate::introspection::Introspection;
use crate::plugin::DynPlugin;
use crate::plugins::traffic_shaping::TrafficShaping;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::query_planner::BridgeQueryPlanner;
use crate::query_planner::CachingQueryPlanner;
use crate::router_factory::Endpoint;
use crate::router_factory::TransportServiceFactory;
use crate::services::layers::ensure_query_presence::EnsureQueryPresence;
use crate::Configuration;
use crate::Context;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::ListenAddr;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::Schema;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// An [`IndexMap`] of available plugins.
pub(crate) type Plugins = IndexMap<String, Box<dyn DynPlugin>>;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct SupergraphService<ExecutionFactory> {
    execution_service_factory: ExecutionFactory,
    query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
    schema: Arc<Schema>,
}

#[buildstructor::buildstructor]
impl<ExecutionFactory> SupergraphService<ExecutionFactory> {
    #[builder]
    pub(crate) fn new(
        query_planner_service: CachingQueryPlanner<BridgeQueryPlanner>,
        execution_service_factory: ExecutionFactory,
        schema: Arc<Schema>,
    ) -> Self {
        SupergraphService {
            query_planner_service,
            execution_service_factory,
            schema,
        }
    }
}

impl<ExecutionFactory> Service<SupergraphRequest> for SupergraphService<ExecutionFactory>
where
    ExecutionFactory: ExecutionServiceFactory,
{
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
        let execution = self.execution_service_factory.new_service();

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
        // FIXME: Enable it later
        // .and_then(|mut res| async move {
        //     if let Some(trace_id) = TraceId::maybe_new().map(|t| t.to_string()) {
        //         let header_value = HeaderValue::from_str(trace_id.as_str());
        //         if let Ok(header_value) = header_value {
        //             res.response
        //                 .headers_mut()
        //                 .insert(HeaderName::from_static("apollo_trace_id"), header_value);
        //         }
        //     }

        //     Ok(res)
        // });

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
            if is_deferred && !accepts_multipart(req.supergraph_request.headers()) {
                let mut response = SupergraphResponse::new_from_graphql_response(graphql::Response::builder()
                    .errors(vec![crate::error::Error::builder()
                        .message(String::from("the router received a query with the @defer directive but the client does not accept multipart/mixed HTTP responses. To enable @defer support, add the HTTP header 'Accept: multipart/mixed; deferSpec=20220824'"))
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
                        ExecutionRequest::builder()
                            .supergraph_request(req.supergraph_request)
                            .query_plan(plan.clone())
                            .context(context)
                            .build(),
                    )
                    .await?;

                let ExecutionResponse { response, context } = execution_response;

                let (parts, response_stream) = response.into_parts();

                Ok(SupergraphResponse {
                    context,
                    response: http::Response::from_parts(
                        parts,
                        response_stream.in_current_span().boxed(),
                    ),
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
        .instrument(tracing::info_span!("query_planning",
            graphql.document = body.query.clone().expect("the query presence was already checked by a plugin").as_str(),
            graphql.operation.name = body.operation_name.clone().unwrap_or_default().as_str(),
            "otel.kind" = %SpanKind::Internal
        ))
        .await
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
}
