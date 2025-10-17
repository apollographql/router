use apollo_router::graphql::Request;
use apollo_router::plugin::test::MockSubgraph;
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn empty_document() {
    let request = Request::fake_builder()
        .query("# intentionally left blank")
        .build();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "parsing error: syntax error: Unexpected <EOF>.",
          "locations": [
            {
              "line": 1,
              "column": 27
            }
          ],
          "extensions": {
            "code": "PARSING_ERROR"
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
        .build();
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
    let request = Request::fake_builder().query("{ me { id } }").build();
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
        .build();
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
        .build();
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
        .build();
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
        .build();
    let response = make_request(request).await;
    insta::assert_json_snapshot!(response, @r###"
    {
      "errors": [
        {
          "message": "Unknown operation named \"SecretThirdOp\"",
          "extensions": {
            "code": "GRAPHQL_UNKNOWN_OPERATION_NAME"
          }
        }
      ]
    }
    "###);
}

async fn make_request(request: Request) -> apollo_router::graphql::Response {
    let router = apollo_router::TestHarness::builder()
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
        .build_router()
        .await
        .unwrap();

    let request = apollo_router::services::router::Request::fake_builder()
        .body(serde_json::to_string(&request).unwrap().into_bytes())
        .method(http::Method::POST)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .build()
        .unwrap();

    let response = router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .expect("should have one response")
        .unwrap();

    serde_json::from_slice(&response).unwrap()
}
