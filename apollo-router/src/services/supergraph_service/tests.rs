
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
        .query("query { currentUser { activeOrganization { nonNullId creatorUser { name } } } }")
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
