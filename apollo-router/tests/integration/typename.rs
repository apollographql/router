use apollo_router::services::supergraph::Request;
use serde_json::json;
use tower::ServiceExt;

const SCHEMA: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
  query: MyQuery
  mutation: MyMutation
}

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(
  graph: join__Graph
  requires: join__FieldSet
  provides: join__FieldSet
  type: String
  external: Boolean
  override: String
  usedOverridden: Boolean
) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__type(
  graph: join__Graph!
  key: join__FieldSet
  extension: Boolean! = false
  resolvable: Boolean! = true
  isInterfaceObject: Boolean! = false
) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(
  graph: join__Graph!
  member: String!
) repeatable on UNION

directive @link(
  url: String
  as: String
  for: link__Purpose
  import: [link__Import]
) repeatable on SCHEMA

directive @join__implements(
  graph: join__Graph!
  interface: String!
) repeatable on OBJECT | INTERFACE

scalar join__FieldSet
scalar link__Import

enum join__Graph {
  SUBGRAPH_A
    @join__graph(
      name: "subgraph-a"
      url: "http://graphql.subgraph-a.svc.cluster.local:4000"
    )
}

enum link__Purpose {
  SECURITY
  EXECUTION
}

type MyMutation @join__type(graph: SUBGRAPH_A) {
  createThing: String
}

type MyQuery @join__type(graph: SUBGRAPH_A) {
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

/* FIXME: should be fixed in query planner, failing with:
   > value retrieval failed: empty query plan. This behavior is unexpected and we suggest opening an issue to apollographql/router with a reproduction.

#[tokio::test]
async fn inside_inline_fragment() {
    let request = Request::fake_builder()
        .query("{ ... { __typename } }")
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
async fn inside_fragment() {
    let query = r#"
       { ...SomeFragment }

       fragment SomeFragment on MyQuery {
         __typename
       }
    "#;
    let request = Request::fake_builder().query(query).build().unwrap();
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
async fn deeply_nested_inside_fragments() {
    let query = r#"
       { ...SomeFragment }

       fragment SomeFragment on MyQuery {
         ... {
           ...AnotherFragment
         }
       }

       fragment AnotherFragment on MyQuery {
         __typename
       }
    "#;
    let request = Request::fake_builder().query(query).build().unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "n": "MyQuery"
      }
    }
    "###);
}
*/

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
