use apollo_router::_private::create_test_service_factory_from_yaml;
use tower::ServiceExt;

#[tokio::test]
async fn test_supergraph_validation_errors_are_passed_on() {
    create_test_service_factory_from_yaml(
        include_str!("../../src/testdata/invalid_supergraph.graphql"),
        "supergraph:\n  introspection: true\n",
    )
    .await;
}

/// <https://github.com/apollographql/router/issues/3388>
#[tokio::test]
async fn test_request_extensions_is_null() {
    // `extensions` is optional:
    // <https://graphql.github.io/graphql-over-http/draft/#sec-Request-Parameters>

    // > Specifying null for optional request parameters is equivalent to not specifying them at all
    // https://graphql.github.io/graphql-over-http/draft/#note-22957

    let request =
        serde_json::json!({"query": "{__typename}", "extensions": serde_json::Value::Null});
    let request = apollo_router::services::router::Request::fake_builder()
        .body(request.to_string())
        .method(hyper::Method::POST)
        .header("content-type", "application/json")
        .build()
        .unwrap();
    let response = apollo_router::TestHarness::builder()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();
    // Used to be an INVALID_GRAPHQL_REQUEST error with "invalid type: null, expected a map"
    assert_eq!(
        String::from_utf8_lossy(&response),
        r#"{"data":{"__typename":"Query"}}"#
    );
}

/// <https://github.com/apollographql/router/issues/3388>
#[tokio::test]
async fn test_syntax_error() {
    let request = serde_json::json!({"query": "{__typename"});
    let request = apollo_router::services::router::Request::fake_builder()
        .body(request.to_string())
        .method(hyper::Method::POST)
        .header("content-type", "application/json")
        .build()
        .unwrap();
    let response = apollo_router::TestHarness::builder()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();

    let v: serde_json::Value = serde_json::from_slice(&response).unwrap();
    insta::assert_json_snapshot!(v, @r###"
    {
      "errors": [
        {
          "message": "syntax error: expected R_CURLY, got EOF",
          "locations": [
            {
              "line": 1,
              "column": 12
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
