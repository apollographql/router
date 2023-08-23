use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use serde_json_bytes::json;
use tower::ServiceExt;

use crate::graphql;
use crate::plugin::test::MockSubgraph;
use crate::services::router;
use crate::services::supergraph;
use crate::Context;
use crate::MockedSubgraphs;
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
   orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__owner(graph: USER)
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id"){
   id: ID!
   name: String
   phone: String
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
async fn authenticated_request() {
    let subgraphs = MockedSubgraphs([
    ("user", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name phone}}}",
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

directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar join__FieldSet
enum join__Graph {
   USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
   ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
}
type Query {
   currentUser: User @join__field(graph: USER)
   orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__owner(graph: USER)
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id"){
   id: ID!
   name: String
   phone: String @authenticated
   activeOrganization: Organization
}
type Organization
@join__owner(graph: ORGA)
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
            serde_json::json! {{ "data": {"_entities":[{"name":"Ada" }] }}},
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
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "preview_directives": {
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

    insta::assert_json_snapshot!(response);
}

const SCOPES_SCHEMA: &str = r#"schema
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

directive @requiresScopes(scopes: [[String!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar join__FieldSet
enum join__Graph {
   USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
   ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
}
type Query {
   currentUser: User @join__field(graph: USER)
   orga(id: ID): Organization @join__field(graph: ORGA)
}
type User
@join__owner(graph: USER)
@join__type(graph: ORGA, key: "id")
@join__type(graph: USER, key: "id")
@requiresScopes(scopes: [["user:read"], ["admin"]]) {
   id: ID!
   name: String
   phone: String @requiresScopes(scopes: [["pii"]])
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
    ).build())
].into_iter().collect());

    let service = TestHarness::builder()
        .configuration_json(serde_json::json!({
        "include_subgraph_errors": {
            "all": true
        },
        "authorization": {
            "preview_directives": {
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
