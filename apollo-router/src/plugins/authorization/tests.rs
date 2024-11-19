use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use serde_json_bytes::json;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::test::MockSubgraph;
use crate::plugin::test::MockSubgraphService;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;
use crate::MockedSubgraphs;
use crate::TestHarness;

const SCHEMA: &str = include_str!("../../testdata/orga_supergraph.graphql");

#[tokio::test]
async fn authenticated_request() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){..._generated_onUser2_0}}fragment _generated_onUser2_0 on User{name phone}",
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
        "experimental_query_planner_mode": "new",
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
        .insert(
            "apollo_authentication::JWT::claims",
            "placeholder".to_string(),
        )
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
        "experimental_query_planner_mode": "new",
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
            "apollo_authentication::JWT::claims",
            json! {{ "scope": "user:read" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(serde_json::to_vec(&req).unwrap().into())
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
}

#[tokio::test]
async fn authenticated_directive_dry_run() {
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
            "apollo_authentication::JWT::claims",
            json! {{ "scope": "user:read" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(serde_json::to_vec(&req).unwrap().into())
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
            "apollo_authentication::JWT::claims",
            json! {{ "scope": "user:read pii" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(serde_json::to_vec(&req).unwrap().into())
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
            "apollo_authentication::JWT::claims",
            json! {{ "scope": "admin" }},
        )
        .unwrap();
    let request = router::Request {
        context,
        router_request: http::Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(serde_json::to_vec(&req).unwrap().into())
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
}

#[tokio::test]
async fn scopes_directive_dry_run() {
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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
}

#[tokio::test]
async fn errors_in_extensions() {
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
        "experimental_query_planner_mode": "new",
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
            .body(serde_json::to_vec(&req).unwrap().into())
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

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_query_planner_mode": "new",
            "authorization": {
                "directives": {
                    "enabled": true
                }
            }
        }))
        .unwrap()
        .schema(CACHE_KEY_SCHEMA)
        .subgraph_hook(|_name, _service| {
            let mut mock_subgraph_service = MockSubgraphService::new();
            mock_subgraph_service.expect_call().times(1).returning(
                move |req: subgraph::Request| {
                    assert_eq!(
                        *req.authorization,
                        CacheKeyMetadata {
                            is_authenticated: true,
                            scopes: vec!["id".to_string()],
                            policies: vec![]
                        }
                    );

                    Ok(subgraph::Response::fake_builder()
                        .context(req.context)
                        .data(serde_json::json! {{

                                "currentUser": {
                                    "id": 1,
                                    "name": "A", // This will be filtered because we don't have the policy
                                    "phone": "1234"
                                }

                        }})
                        .build())
                },
            );
            mock_subgraph_service.boxed()
        })
        .build_router()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(
            "apollo_authentication::JWT::claims",
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
}
