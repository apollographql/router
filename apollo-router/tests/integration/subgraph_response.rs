use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;

const CONFIG: &str = r#"
include_subgraph_errors:
  all: true
"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_returning_data_null() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({ "data": null })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = "{ __typename topProducts { name } }";
    let (_trace_id, response) = router.execute_query(&json!({ "query":  query })).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": null })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_returning_different_typename_on_query_root() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
           "data": {
               "topProducts": null,
               "__typename": "SomeQueryRoot",
               "aliased": "SomeQueryRoot",
               "inside_fragment": "SomeQueryRoot",
               "inside_inline_fragment": "SomeQueryRoot"
           }
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = r#"
        {
            topProducts { name }
            __typename
            aliased: __typename
            ...TypenameFragment
            ... {
                inside_inline_fragment: __typename
            }
        }

        fragment TypenameFragment on Query {
            inside_fragment: __typename
        }
    "#;
    let (_trace_id, response) = router.execute_query(&json!({ "query":  query })).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": {
                "topProducts": null,
                "__typename": "Query",
                "aliased": "Query",
                "inside_fragment": "Query",
                "inside_inline_fragment": "Query"
            }
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_valid_extensions_service_for_subgraph_error() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "path": ["topProducts"]
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "path":["topProducts"],
                "extensions": {
                    "service": "products"
                }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_valid_extensions_service_is_preserved_for_subgraph_error() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "path": ["topProducts"],
                "extensions": {
                    "service": 3.14,
                }
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "path":["topProducts"],
                "extensions": {
                    "service": 3.14
                }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_valid_extensions_service_for_invalid_subgraph_response() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
          "data": null,
          "errors": [
            {
              "message": "HTTP fetch failed from 'products': subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json",
              "path": [],
              "extensions": {
                "code": "SUBREQUEST_HTTP_ERROR",
                "service": "products",
                "reason": "subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json",
                "http": { "status": 200 }
              }
            }
          ]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_valid_error_locations() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [
                    { "line": 1, "column": 2 },
                    { "line": 3, "column": 4 },
                ],
                "path": ["topProducts"]
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "locations": [
                    { "line": 1, "column": 2 },
                    { "line": 3, "column": 4 },
                ],
                "path":["topProducts"],
                "extensions": { "service": "products" }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_empty_error_locations() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [],
                "path": ["topProducts"]
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "path":["topProducts"],
                "extensions": { "service": "products" }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_error_locations() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [{ "line": true }],
                "path": ["topProducts"]
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": null,
            "errors": [{
                "message":"service 'products' response was malformed: invalid `locations` within error: invalid type: boolean `true`, expected u32",
                "path": [],
                "extensions": {
                    "service": "products",
                    "reason": "invalid `locations` within error: invalid type: boolean `true`, expected u32",
                    "code": "SUBREQUEST_MALFORMED_RESPONSE",
                    "service": "products"
                }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_error_locations_with_single_negative_one_location() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [{ "line": -1, "column": -1 }],
                "path": ["topProducts"],
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "path":["topProducts"],
                "extensions": { "service": "products" }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_error_locations_contains_negative_one_location() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [
                    { "line": 1, "column": 2 },
                    { "line": -1, "column": -1 },
                    { "line": 3, "column": 4 },
                ],
                "path": ["topProducts"]
            }]
        })))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({ "query": "{ topProducts { name } }" }))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "locations": [
                    { "line": 1, "column": 2 },
                    { "line": 3, "column": 4 },
                ],
                "path":["topProducts"],
                "extensions": { "service": "products" }
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}
