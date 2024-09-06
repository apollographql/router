use apollo_router::services::supergraph::Request;
use serde_json::json;
use tower::ServiceExt;

const SCHEMA: &str = r#"
schema
  @core(feature: "https://specs.apollo.dev/core/v0.1"),
  @core(feature: "https://specs.apollo.dev/join/v0.1")
{
  query: MyQuery
  mutation: MyMutation
}

directive @core(feature: String!) repeatable on SCHEMA

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION

directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE

directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

scalar join__FieldSet

enum join__Graph {
  ACCOUNTS @join__graph(name: "accounts" url: "http://localhost:4001")
  INVENTORY @join__graph(name: "inventory" url: "http://localhost:4004")
  PRODUCTS @join__graph(name: "products" url: "http://localhost:4003")
  REVIEWS @join__graph(name: "reviews" url: "http://localhost:4002")
}

type MyMutation {
  createThing: String
}

type MyQuery {
  thing: String
}
"#;

#[tokio::test]
async fn basic() {
    let request = Request::fake_builder()
        .query("{ __typename }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__typename": "MyQuery"
      }
    }
    "###);
}

#[tokio::test]
async fn aliased() {
    let request = Request::fake_builder()
        .query("{ n: __typename }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "n": "MyQuery"
      }
    }
    "###);
}

#[tokio::test]
async fn mutation() {
    let request = Request::fake_builder()
        .query("mutation { __typename }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__typename": "MyMutation"
      }
    }
    "###);
}

#[tokio::test]
async fn two_named_operations() {
    let request = Request::fake_builder()
        .query(
            r#"
                mutation Op { __typename }
                query OtherOp { __typename }
            "#,
        )
        .operation_name("OtherOp")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__typename": "MyQuery"
      }
    }
    "###);
}

async fn make_request(request: Request) -> apollo_router::graphql::Response {
    apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "include_subgraph_errors": {
                "all": true,
            },
        }))
        .unwrap()
        .schema(SCHEMA)
        .build_supergraph()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
}
