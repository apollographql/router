use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph::Request;
use serde_json::json;
use tower::ServiceExt;

use crate::integration::IntegrationTest;

#[tokio::test]
async fn simple_legacy_mode() {
    let request = Request::fake_builder()
        .query("{ __schema { queryType { name } } }")
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn simple_new_mode() {
    let request = Request::fake_builder()
        .query("{ __schema { queryType { name } } }")
        .build()
        .unwrap();
    let response = make_request(request, "new").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn top_level_inline_fragment() {
    let request = Request::fake_builder()
        .query("{ ... { __schema { queryType { name } } } }")
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "__schema": {
          "queryType": {
            "name": "Query"
          }
        }
      }
    }
    "###);
}

#[tokio::test]
async fn variable() {
    let request = Request::fake_builder()
        .query(
            r#"
                query($d: Boolean!) {
                    __type(name: "Query") {
                        fields(includeDeprecated: $d) {
                            name
                        }
                    }
                }
            "#,
        )
        .variable("d", true)
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "introspection error : Variable \"$d\" of required type \"Boolean!\" was not provided.",
          "extensions": {
            "code": "INTROSPECTION_ERROR"
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn two_operations() {
    let request = Request::fake_builder()
        .query(
            r#"
                query ThisOp { __schema { queryType { name } } }
                query OtherOp { me { id } }
            "#,
        )
        .operation_name("ThisOp")
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Schema introspection is currently not supported with multiple operations in the same document",
          "extensions": {
            "code": "INTROSPECTION_WITH_MULTIPLE_OPERATIONS"
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn operation_name_error() {
    let request = Request::fake_builder()
        .query(
            r#"
                query ThisOp { me { id } }
                query OtherOp { me { id } }
            "#,
        )
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Must provide operation name if query contains multiple operations.",
          "extensions": {
            "code": "GRAPHQL_VALIDATION_FAILED"
          }
        }
      ]
    }
    "###);

    let request = Request::fake_builder()
        .query("query ThisOp { me { id } }")
        .operation_name("NonExistentOp")
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Unknown operation named \"NonExistentOp\"",
          "extensions": {
            "code": "GRAPHQL_VALIDATION_FAILED"
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn mixed() {
    let request = Request::fake_builder()
        .query(
            r#"{
                __schema { queryType { name } }
                me { id }
            }"#,
        )
        .build()
        .unwrap();
    let response = make_request(request, "legacy").await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Mixed queries with both schema introspection and concrete fields are not supported",
          "extensions": {
            "code": "MIXED_INTROSPECTION"
          }
        }
      ]
    }
    "###);
}

async fn make_request(request: Request, mode: &str) -> apollo_router::graphql::Response {
    apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_introspection_mode": mode,
            "supergraph": {
                "introspection": true,
            },
            "include_subgraph_errors": {
                "all": true,
            },
        }))
        .unwrap()
        .subgraph_hook(|subgraph_name, default| match subgraph_name {
            "accounts" => MockSubgraph::builder()
                .with_json(
                    json!({"query": "{me{id}}"}),
                    json!({"data": {"me": {"id": 1}}}),
                )
                .build()
                .boxed(),
            _ => default,
        })
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

#[tokio::test]
async fn both_mode_integration() {
    let mut router = IntegrationTest::builder()
        .config(
            "
                # `experimental_introspection_mode` now defaults to `both`
                supergraph:
                    introspection: true
            ",
        )
        .supergraph("../examples/graphql/local.graphql")
        .log("error,apollo_router=info,apollo_router::query_planner=debug")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .execute_query(&json!({
            "query": include_str!("../fixtures/introspect_full_schema.graphql"),
        }))
        .await;
    router.assert_log_contains("Introspection match! 🎉").await;
    router.graceful_shutdown().await;
}
