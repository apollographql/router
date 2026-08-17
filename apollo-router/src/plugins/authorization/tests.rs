use std::sync::Arc;
use std::sync::Mutex;

use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use regex::Regex;
use serde_json_bytes::json;
use tokio::task::JoinHandle;
use tower::ServiceExt;

use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;
use crate::apollo_studio_interop::UsageReporting;
use crate::graphql;
use crate::plugin::test::MockSubgraph;
use crate::plugins::authorization::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::router;
use crate::services::router::body;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::test_harness::tracing_test;

const SCHEMA: &str = include_str!("../../testdata/orga_supergraph.graphql");

fn assert_span_contains_authorization_error_event(span: &str) {
    let pattern = format!(
        r"^[0-9TZ\-:.]+ ERROR router\{{[^}}]+}}:{span}.*Authorization error unauthorized_query_paths=\[.*]$"
    );
    let span_regex = Regex::new(&pattern).unwrap();

    let contains_err_event_in_span = tracing_test::logs_assert(|lines| {
        for line in lines {
            if span_regex.captures(line).is_some() {
                return Ok(());
            }
        }

        Err(lines.join("\n"))
    });
    assert!(contains_err_event_in_span.is_ok());
}

fn assert_logs_contain_entire_request_authorization_error() {
    assert_span_contains_authorization_error_event("query_planning");
}

fn assert_logs_contain_partial_authorization_error() {
    assert_span_contains_authorization_error_event("format_response");
}

#[tokio::test]
async fn authenticated_request() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){... on User{name phone}}}",
                "variables": {
                    "representations": [
                        { "__typename": "User", "id":0 }
                    ],
                }
            }},
            serde_json::json! {{
                "data": {
                    "_entities":[
                        {
                            "name":"Ada",
                            "phone": "1234"
                        }
                    ]
                }
            }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "require_authentication": true
        }}))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(APOLLO_AUTHENTICATION_JWT_CLAIMS, "placeholder".to_string())
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
        .variables(
            json! {{ "isAuthenticated": true }}
                .as_object()
                .unwrap()
                .clone(),
        )
        .context(context)
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
async fn unauthenticated_request() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {
                    "representations": [
                        { "__typename": "User", "id":0 }
                    ],
                }
            }},
            serde_json::json! {{
                "data": {
                    "_entities":[
                        {
                            "name":"Ada"
                        }
                    ]
                }
            }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "require_authentication": true
        }}))
        .unwrap()
        .schema(SCHEMA)
        .extra_plugin(subgraphs)
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    let request = supergraph::Request::fake_builder()
        .query("query { orga(id: 1) { id creatorUser { id name phone } } }")
        .variables(
            json! {{ "isAuthenticated": false }}
                .as_object()
                .unwrap()
                .clone(),
        )
        .context(context)
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

const AUTHENTICATED_SCHEMA: &str = r#"schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
  {
  query: Query
}
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

scalar link__Import
enum link__Purpose {
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY
  
    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
  }

directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar join__FieldSet
enum join__Graph {
   USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
   ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
}

type Query
@join__type(graph: ORGA)
@join__type(graph: USER){
   currentUser: User @join__field(graph: USER)
   orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id"){
   id: ID!
   name: String
   phone: String @authenticated
   activeOrganization: Organization
}
type Organization
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id") {
   id: ID @authenticated
   creatorUser: User
   name: String
   nonNullId: ID!
   suborga: [Organization]
}"#;

#[tokio::test]
async fn authenticated_directive() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{ "name":"Ada" }] }}},
        )
        .with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{"name":"Ada", "phone": "1234"}] }}},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "id": 0, "name":"Ada" } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": {"id": 0, "name":"Ada", "phone": "1234" } }}}}
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true
            }
        }}))
        .unwrap()
        .schema(AUTHENTICATED_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        variables: json! {{ "isAuthenticated": false }}
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };

    let context = Context::new();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);

    let context = Context::new();
    context
        .insert(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json! {{ "scope": "user:read" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    println!("req2");

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn authenticated_directive_reject_unauthorized() {
    let _guard = tracing_test::dispatcher_guard();

    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{ "name":"Ada" }] }}},
        )
        .with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{"name":"Ada", "phone": "1234"}] }}},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "id": 0, "name":"Ada" } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": {"id": 0, "name":"Ada", "phone": "1234" } }}}}
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true,
                "reject_unauthorized": true
            }
        }}))
        .unwrap()
        .schema(AUTHENTICATED_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        variables: json! {{ "isAuthenticated": false }}
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };

    let context = Context::new();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
    assert_logs_contain_entire_request_authorization_error();
}

/// A subgraph can return more than the operation selected, so the data reaching response
/// formatting is not bounded by what authorization left in the query. `ExecutionService`
/// handles that by formatting twice: the filtered query projects the data onto the
/// authorized shape, then the original query expands it to the shape the client asked
/// for.
///
/// `User.phone` is `@authenticated`, so filtering removes it from an unauthenticated
/// operation while this subgraph returns it anyway. `Some(Value::Null)` pins both halves
/// of that arrangement in one assertion: `phone` is present, so the original query
/// restored the requested shape, and it is null rather than `"1234"`, so the filtered
/// query stripped the value the client may not see.
///
/// Removing either property changes this key: drop the filtered pass and it holds
/// `"1234"`, run the passes in the other order and it disappears from the response.
#[tokio::test]
async fn overfetched_unauthorized_field_is_not_returned() {
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "authorization": { "directives": { "enabled": true } }
        }))
        .unwrap()
        .schema(AUTHENTICATED_SCHEMA)
        .subgraph_hook(|_name, _service| {
            let (mock, mut handle) =
                tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
            tokio::spawn(async move {
                while let Some((req, responder)) = handle.next_request().await {
                    // `phone` is not in the filtered operation this subgraph was sent.
                    responder.send_response(
                        subgraph::Response::fake_builder()
                            .context(req.context)
                            .data(serde_json::json! {{
                                "currentUser": { "name": "Ada", "phone": "1234" }
                            }})
                            .build(),
                    );
                }
            });
            mock.boxed_clone()
        })
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query("query { currentUser { name phone } }")
        .context(Context::new())
        .build()
        .unwrap();
    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();

    let current_user = response
        .data
        .as_ref()
        .expect("the operation kept `name`, so it must not reject outright")
        .get("currentUser")
        .expect("`currentUser` must survive; only `phone` is @authenticated");

    assert_eq!(
        current_user.get("phone"),
        Some(&serde_json_bytes::Value::Null),
        "the subgraph returned `phone` outside the filtered operation, so it must reach \
         the client as null rather than as its value"
    );
    assert_eq!(current_user.get("name"), Some(&json!("Ada")));
}

mod whole_query_rejection {
    use super::*;

    /// `Organization.id` and `User.phone` are both `@authenticated`, so filtering removes
    /// paths from an unauthenticated request and `reject_unauthorized` turns that into a
    /// whole-query rejection.
    const REJECTED_QUERY: &str = "query { orga(id: 1) { id creatorUser { id name phone } } }";

    type SubgraphHandles =
        Arc<Mutex<Vec<tower_test::mock::Handle<subgraph::Request, subgraph::Response>>>>;

    /// Builds a router that rejects `REJECTED_QUERY`, replacing every subgraph with a
    /// `tower_test` mock. The mocks hold no canned responses, so reaching one fails.
    async fn build_router_rejecting_whole_query() -> (router::BoxCloneService, SubgraphHandles) {
        let handles: SubgraphHandles = Arc::new(Mutex::new(Vec::new()));
        let handles_clone = handles.clone();

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "authorization": {
                    "directives": {
                        "enabled": true,
                        "reject_unauthorized": true
                    }
                }
            }))
            .unwrap()
            .schema(AUTHENTICATED_SCHEMA)
            .subgraph_hook(move |_name, _service| {
                let (mock, handle) =
                    tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
                handles_clone.lock().unwrap().push(handle);
                mock.boxed_clone()
            })
            .build_router()
            .await
            .unwrap();

        (service, handles)
    }

    /// Fails if any subgraph mock received a request, or if the router built no subgraph
    /// service at all, which would make the check vacuous.
    async fn assert_no_subgraph_calls(handles: SubgraphHandles) {
        let handles: Vec<_> = handles.lock().unwrap().drain(..).collect();
        assert!(!handles.is_empty(), "no subgraph services were created");
        for handle in handles {
            crate::plugin::test::assert_no_mock_calls(handle).await;
        }
    }

    fn rejected_request(context: Context) -> router::Request {
        let req = graphql::Request {
            query: Some(REJECTED_QUERY.to_string()),
            ..Default::default()
        };
        router::Request {
            context,
            router_request: http::Request::builder()
                .method("POST")
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
                .unwrap(),
        }
    }

    /// Sends `REJECTED_QUERY`, asserts the router rejected it on authorization grounds,
    /// and returns the HTTP status.
    async fn send_rejected_request(
        service: router::BoxCloneService,
        context: Context,
    ) -> http::StatusCode {
        let response = service.oneshot(rejected_request(context)).await.unwrap();
        let status = response.response.status();

        let body = response
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            body.errors.first().map(|e| e.message.as_str()),
            Some("Unauthorized field or type"),
            "the operation was not rejected on authorization grounds"
        );

        status
    }

    #[tokio::test]
    async fn does_not_reach_execution() {
        let (service, handles) = build_router_rejecting_whole_query().await;

        send_rejected_request(service, Context::new()).await;

        assert_no_subgraph_calls(handles).await;
    }

    /// The status comes from the short-circuit in the query planner, not from response
    /// formatting: `filter_query` returns `Err(Unauthorized)`, `QueryPlannerService::get`
    /// builds a `graphql::Response` with `data: null` directly, and
    /// `SupergraphResponse::new_from_graphql_response` wraps it with `http::Response::new`,
    /// which is 200 regardless of errors or data shape. Execution and value completion never
    /// run — see `does_not_reach_execution`. 200 with errors is the right answer for a
    /// rejection with field-error semantics, so this pins that the short-circuit does not
    /// pick up an error status along the way.
    #[tokio::test]
    async fn returns_http_200() {
        let (service, _handles) = build_router_rejecting_whole_query().await;

        let status = send_rejected_request(service, Context::new()).await;

        assert_eq!(status, http::StatusCode::OK);
    }

    /// `CachingQueryPlanner` records usage reporting only for the `Plan` variant.
    /// Telemetry still meters the rejection as one licensed operation but sends no
    /// signature, referenced fields, or per-type stats, so Studio cannot attribute it.
    #[tokio::test]
    async fn does_not_record_usage_reporting() {
        let (service, _handles) = build_router_rejecting_whole_query().await;
        let context = Context::new();

        send_rejected_request(service, context.clone()).await;

        assert!(
            !context
                .extensions()
                .with_lock(|lock| lock.contains_key::<Arc<UsageReporting>>())
        );
    }
}

#[tokio::test]
async fn authenticated_directive_dry_run() {
    let _guard = tracing_test::dispatcher_guard();
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{ "name":"Ada" }] }}},
        )
        .with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": {"_entities":[{"name":"Ada", "phone": "1234"}] }}},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "creatorUser": { "id": 0, "name":"Ada" } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": {"id": 0, "name":"Ada", "phone": "1234" } }}}}
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true,
                "dry_run": true
            }
        }}))
        .unwrap()
        .schema(AUTHENTICATED_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        variables: json! {{ "isAuthenticated": false }}
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };

    let context = Context::new();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
    assert_logs_contain_partial_authorization_error();
}

const SCOPES_SCHEMA: &str = r#"schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
  {
    query: Query
}
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

scalar link__Import
enum link__Purpose {
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY
  
    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
  }

scalar federation__Scope
directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar join__FieldSet
enum join__Graph {
   USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
   ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
}

type Query
@join__type(graph: ORGA)
@join__type(graph: USER){
   currentUser: User @join__field(graph: USER)
   orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id")
@requiresScopes(scopes: [["user:read"], ["admin"]]) {
   id: ID!
   name: String
   phone: String @requiresScopes(scopes: [["pii"]])
   activeOrganization: Organization
}
type Organization
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id") {
   id: ID
   creatorUser: User
   name: String
   nonNullId: ID!
   suborga: [Organization]
}"#;

#[tokio::test]
async fn scopes_directive() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada"}] } }},
        ).with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada", "phone": "1234"}] } }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1 }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada" } }}}}
    )
    .with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada", "phone": "1234" } }}}}
    )
    .build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true
            }
        }}))
        .unwrap()
        .schema(SCOPES_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        ..Default::default()
    };
    let request = router::Request {
        context: Context::new(),
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);

    let context = Context::new();
    context
        .insert(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json! {{ "scope": "user:read" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);

    let context = Context::new();
    context
        .insert(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json! {{ "scope": "user:read pii" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);

    let context = Context::new();
    context
        .insert(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json! {{ "scope": "admin" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
}

#[tokio::test]
async fn scopes_directive_reject_unauthorized() {
    let _guard = tracing_test::dispatcher_guard();

    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada"}] } }},
        ).with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada", "phone": "1234"}] } }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1 }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada" } }}}}
    )
    .with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada", "phone": "1234" } }}}}
    )
    .build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true,
                "reject_unauthorized": true,
            }
        }}))
        .unwrap()
        .schema(SCOPES_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        ..Default::default()
    };
    let request = router::Request {
        context: Context::new(),
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
    assert_logs_contain_entire_request_authorization_error();
}

#[tokio::test]
async fn scopes_directive_dry_run() {
    let _guard = tracing_test::dispatcher_guard();
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada"}] } }},
        ).with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada", "phone": "1234"}] } }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1 }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada" } }}}}
    )
    .with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada", "phone": "1234" } }}}}
    )
    .build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true,
                "dry_run": true,
            }
        }}))
        .unwrap()
        .schema(SCOPES_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        ..Default::default()
    };
    let request = router::Request {
        context: Context::new(),
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
    assert_logs_contain_partial_authorization_error();
}

#[tokio::test]
async fn errors_in_extensions() {
    let _guard = tracing_test::dispatcher_guard();
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada"}] } }},
        ).with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
                "variables": {"representations": [{ "__typename": "User", "id":0 }],}
            }},
            serde_json::json! {{ "data": { "_entities":[{"name":"Ada", "phone": "1234"}] } }},
        ).build()),
    ("orga", MockSubgraph::builder().with_json(
        serde_json::json!{{"query":"{orga(id:1){id}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1 }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{__typename id}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "__typename": "User", "id": 0 } }}}}
    ).with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada" } }}}}
    )
    .with_json(
        serde_json::json!{{"query":"{orga(id:1){id creatorUser{id name phone}}}"}},
        serde_json::json!{{"data": {"orga": { "id": 1, "creatorUser": { "id": 0, "name":"Ada", "phone": "1234" } }}}}
    )
    .build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "directives": {
                "enabled": true,
                "errors": {
                    "response": "extensions"
                }
            }
        }}))
        .unwrap()
        .schema(SCOPES_SCHEMA)
        .extra_plugin(subgraphs)
        .build_router()
        .await
        .unwrap();

    let req = graphql::Request {
        query: Some("query { orga(id: 1) { id creatorUser { id name phone } } }".to_string()),
        ..Default::default()
    };
    let request = router::Request {
        context: Context::new(),
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body::from_bytes(serde_json::to_vec(&req).unwrap()))
            .unwrap(),
    };

    let response = service
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();

    insta::assert_json_snapshot!(response);
    assert_logs_contain_partial_authorization_error();
}

const CACHE_KEY_SCHEMA: &str = r#"schema
@link(url: "https://specs.apollo.dev/link/v1.0")
@link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
@link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
@link(url: "https://specs.apollo.dev/requiresScopes/v0.1", for: SECURITY)
@link(url: "https://specs.apollo.dev/policy/v0.1", for: SECURITY)

{
query: Query
}
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

scalar link__Import
enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY

  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
scalar federation__Scope
directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
directive @policy(policies: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar join__FieldSet
enum join__Graph {
 USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
 ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
}

type Query
@join__type(graph: ORGA)
@join__type(graph: USER){
 currentUser: User @join__field(graph: USER)
 orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id"){
 id: ID! @requiresScopes(scopes: [["id"]])
 name: String @policy(policies: [["name"]])
 phone: String @authenticated
 activeOrganization: Organization
}
type Organization
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id") {
 id: ID @authenticated
 creatorUser: User
 name: String
 nonNullId: ID!
 suborga: [Organization]
}"#;

#[tokio::test]
async fn cache_key_metadata() {
    let query = "query { currentUser { id name phone } }";

    let drivers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
    let drivers_clone = drivers.clone();
    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "authorization": {
                "directives": {
                    "enabled": true
                }
            }
        }))
        .unwrap()
        .schema(CACHE_KEY_SCHEMA)
        .subgraph_hook(move |_name, _service| {
            let (mock, mut handle) =
                tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
            let driver = tokio::spawn(async move {
                while let Some((req, responder)) = handle.next_request().await {
                    assert_eq!(
                        *req.authorization,
                        CacheKeyMetadata {
                            is_authenticated: true,
                            scopes: vec!["id".to_string()],
                            policies: vec![]
                        }
                    );
                    // name will be null in the response: it requires @policy(["name"]) which we don't hold
                    responder.send_response(
                        subgraph::Response::fake_builder()
                            .context(req.context)
                            .data(serde_json::json! {{
                                "currentUser": {
                                    "id": 1,
                                    "name": "A",
                                    "phone": "1234"
                                }
                            }})
                            .build(),
                    );
                }
            });
            drivers_clone.lock().unwrap().push(driver);
            mock.boxed_clone()
        })
        .build_router()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(
            APOLLO_AUTHENTICATION_JWT_CLAIMS,
            json! {{ "scope": "id test" }},
        )
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .context(context)
        .build()
        .unwrap();
    let mut response = service
        .oneshot(router::Request::try_from(request).unwrap())
        .await
        .unwrap();
    let response = response.next_response().await.unwrap().unwrap();
    let response: serde_json::Value = serde_json::from_slice(&response).unwrap();

    insta::assert_json_snapshot!(response);

    for driver in Arc::try_unwrap(drivers).unwrap().into_inner().unwrap() {
        crate::plugin::test::await_mock_driver(driver).await;
    }
}

// Verifies that the refactored typed config parsing produces the same values as the
// old manual JSON path navigation for every combination of present/absent config fields.
mod config_parsing {
    use serde_json::json;

    use crate::plugins::authorization::Conf;
    use crate::plugins::authorization::ErrorConfig;

    // Old extraction logic, preserved verbatim from before the refactor.
    fn old_enabled(value: &serde_json::Value) -> Option<bool> {
        value
            .get("directives")
            .and_then(|v| v.as_object())
            .and_then(|v| v.get("enabled").and_then(|v| v.as_bool()))
    }

    fn old_errors(value: &serde_json::Value) -> Option<ErrorConfig> {
        value
            .get("directives")
            .and_then(|v| v.as_object())
            .and_then(|v| {
                v.get("errors")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
            })
    }

    fn old_directives_flags(value: &serde_json::Value) -> Option<(bool, bool)> {
        value
            .get("directives")
            .and_then(|v| v.as_object())
            .map(|config| {
                (
                    config
                        .get("reject_unauthorized")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    config
                        .get("dry_run")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                )
            })
    }

    // New extraction via typed deserialization.
    fn new_conf(value: &serde_json::Value) -> Option<Conf> {
        serde_json::from_value(value.clone()).ok()
    }

    #[rstest::rstest]
    #[case(json!({}))]
    #[case(json!({ "directives": {} }))]
    #[case(json!({ "directives": { "enabled": true } }))]
    #[case(json!({ "directives": { "enabled": false } }))]
    #[case(json!({ "directives": { "dry_run": true } }))]
    #[case(json!({ "directives": { "reject_unauthorized": true } }))]
    #[case(json!({ "directives": { "dry_run": true, "reject_unauthorized": true } }))]
    #[case(json!({ "directives": { "errors": {} } }))]
    #[case(json!({ "directives": { "errors": { "log": false } } }))]
    #[case(json!({ "directives": { "errors": { "response": "extensions" } } }))]
    #[case(json!({ "directives": { "errors": { "response": "disabled" } } }))]
    #[case(json!({ "directives": { "errors": { "log": false, "response": "extensions" } } }))]
    #[case(json!({
        "directives": {
            "enabled": true,
            "dry_run": true,
            "reject_unauthorized": true,
            "errors": { "log": true, "response": "errors" }
        }
    }))]
    fn config_parsing_matches(#[case] value: serde_json::Value) {
        let conf = new_conf(&value);

        let old_enabled = old_enabled(&value).unwrap_or(true);
        let old_errors = old_errors(&value).unwrap_or_default();
        let old_flags = old_directives_flags(&value).unwrap_or((false, false));

        let new_conf = conf.clone().unwrap_or_default();

        let new_enabled = new_conf.directives.enabled;
        let new_errors = new_conf.directives.errors;
        let new_flags = (
            new_conf.directives.reject_unauthorized,
            new_conf.directives.dry_run,
        );

        assert_eq!(old_enabled, new_enabled);
        assert_eq!(old_errors, new_errors);
        assert_eq!(old_flags, new_flags);
    }
}
