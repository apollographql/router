use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;

const CONFIG: &str = r#"
include_subgraph_errors:
  all: true
"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_valid_error_locations() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "me": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [
                    { "line": 0, "column": 1 },
                    { "line": 2, "column": 3 },
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
        serde_json::from_str::<serde_json::Value>(&response.text().await?)?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "locations": [
                    { "line": 0, "column": 1 },
                    { "line": 2, "column": 3 },
                ],
                "path":["topProducts"]
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
            "data": { "me": null },
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
        serde_json::from_str::<serde_json::Value>(&response.text().await?)?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "path":["topProducts"]
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
            "data": { "me": null },
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
        serde_json::from_str::<serde_json::Value>(&response.text().await?)?,
        json!({
            "data": null,
            "errors": [{
                "message":"service 'products' response was malformed: invalid `locations` within error: invalid type: boolean `true`, expected u32",
                "extensions": {
                    "service": "products",
                    "reason": "invalid `locations` within error: invalid type: boolean `true`, expected u32",
                    "code": "SUBREQUEST_MALFORMED_RESPONSE",
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
            "data": { "me": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [{ "line": -1, "column": -1 }],
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
        serde_json::from_str::<serde_json::Value>(&response.text().await?)?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "path":["topProducts"]
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
            "data": { "me": null },
            "errors": [{
                "message": "Some error on subgraph",
                "locations": [
                    { "line": 0, "column": 1 },
                    { "line": -1, "column": -1 },
                    { "line": 2, "column": 3 },
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
        serde_json::from_str::<serde_json::Value>(&response.text().await?)?,
        json!({
            "data": { "topProducts": null },
            "errors": [{
                "message":"Some error on subgraph",
                "locations": [
                    { "line": 0, "column": 1 },
                    { "line": 2, "column": 3 },
                ],
                "path":["topProducts"]
            }]
        })
    );

    router.graceful_shutdown().await;
    Ok(())
}
