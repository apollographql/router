use std::collections::HashMap;

use serde_json::json;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_errors_on_http1_max_headers() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_headers: 100
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    for i in 0..100 {
        headers.insert(format!("test-header-{i}"), format!("value_{i}"));
    }

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .headers(headers)
                .build(),
        )
        .await;
    assert_eq!(response.status(), 431);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_allow_to_change_http1_max_headers() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_headers: 200
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    for i in 0..100 {
        headers.insert(format!("test-header-{i}"), format!("value_{i}"));
    }

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .headers(headers)
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_errors_on_http1_header_that_does_not_fit_inside_buffer()
-> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_buf_size: 100kib
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // HTTP/1 has no frame-level rejection: when the request header (1 MiB + 1) overruns
    // the server's 100 KiB read buffer, hyper sends 431 and immediately closes the
    // connection while the client is still streaming the body. Depending on TCP scheduling
    // the client either reads the 431 response or sees the connection reset before the
    // response surfaces. Both outcomes prove the server rejected the oversized header, so
    // we accept either rather than panicking on the connection error (the
    // connection-reset path has been observed on amd_linux). Going through reqwest directly
    // (instead of `execute_query`) also keeps the harness's panic-on-send-error from
    // racing with the legitimate server-side rejection.
    let url = format!("http://{}", router.bind_address());
    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header("content-type", "application/json")
        .header("test-header", "x".repeat(1048576 + 1))
        .json(&json!({ "query": "{ __typename }" }))
        .send()
        .await
    {
        Ok(response) => assert_eq!(response.status(), 431),
        Err(err) => {
            // The send failed before the 431 could be read — the server still rejected
            // the request, which is what the test is asserting. Sanity-check the error
            // is a transport-level failure (not, e.g., a URL parse error) and continue.
            assert!(
                err.is_request() || err.is_body() || err.is_connect(),
                "unexpected reqwest error variant for oversized-header reject: {err}"
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_allow_to_change_http1_max_buf_size() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_buf_size: 2mib
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", "x".repeat(1048576 + 1))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}
