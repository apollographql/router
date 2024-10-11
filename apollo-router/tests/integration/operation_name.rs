use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::supergraph::Request;
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
#[ignore] // TODO: temporarily ignored, as a breaking change in a dependency without a major version bump causes this to fail in `test_update` on CI builds
async fn empty_document() {
    let request = Request::fake_builder()
        .query("# intentionally left blank")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Syntax Error: Unexpected <EOF>.",
          "extensions": {
            "code": "GRAPHQL_PARSE_FAILED"
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn zero_operation() {
    let request = Request::fake_builder()
        .query("fragment F on Query { me { id }}")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Fragment \"F\" is never used.",
          "locations": [
            {
              "line": 1,
              "column": 1
            }
          ],
          "extensions": {
            "code": "GRAPHQL_VALIDATION_FAILED"
          }
        }
      ]
    }
    "###);
}

#[tokio::test]
async fn anonymous_operation() {
    let request = Request::fake_builder()
        .query("{ me { id } }")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "me": {
          "id": 1
        }
      }
    }
    "###);
}

#[tokio::test]
async fn named_operation() {
    let request = Request::fake_builder()
        .query("query Op { me { id } }")
        .operation_name("Op")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "me": {
          "id": 1
        }
      }
    }
    "###);
}

#[tokio::test]
async fn two_named_operations() {
    let request = Request::fake_builder()
        .query(
            r#"
                query Op { me { id } }
                query OtherOp { me { name } }
            "#,
        )
        .operation_name("Op")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "data": {
        "me": {
          "id": 1
        }
      }
    }
    "###);
}

#[tokio::test]
async fn missing_operation_name() {
    let request = Request::fake_builder()
        .query(
            r#"
                query Op { me { id } }
                query OtherOp { me { name } }
            "#,
        )
        .build()
        .unwrap();
    let response = make_request(request).await;
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
}

#[tokio::test]
async fn incorrect_operation_name() {
    let request = Request::fake_builder()
        .query(
            r#"
                query Op { me { id } }
                query OtherOp { me { name } }
            "#,
        )
        .operation_name("SecretThirdOp")
        .build()
        .unwrap();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Unknown operation named \"SecretThirdOp\"",
          "extensions": {
            "code": "GRAPHQL_VALIDATION_FAILED"
          }
        }
      ]
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
        .subgraph_hook(|subgraph_name, default| match subgraph_name {
            "accounts" => MockSubgraph::builder()
                .with_json(
                    json!({"query": "{me{id}}"}),
                    json!({"data": {"me": {"id": 1}}}),
                )
                .with_json(
                    json!({
                        "query": "query Op__accounts__0{me{id}}",
                        "operationName": "Op__accounts__0",
                    }),
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
