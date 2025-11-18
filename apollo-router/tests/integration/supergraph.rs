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

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", "x".repeat(1048576 + 1))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 431);
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

// New test cases for limits configuration approach with max_headers
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_max_headers_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_headers: 10
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    for i in 0..11 {
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
async fn test_supergraph_limits_max_headers_within_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_headers: 50
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    for i in 0..30 {
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

// Test for individual header size limits (http_max_header_size)
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_http_max_header_size_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http_max_header_size: 1kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create a header value larger than 1KB
    let large_header_value = "x".repeat(2048);

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", large_header_value)
                .build(),
        )
        .await;

    // Should return 431 Request Header Fields Too Large or 400 Bad Request
    assert!(response.status() == 431 || response.status() == 400);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_http_max_header_size_within_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http_max_header_size: 2kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create a header value smaller than 2KB
    let header_value = "x".repeat(1000);

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", header_value)
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

// Test for HTTP/2 header list size limits (http_max_header_list_size)
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_http_max_header_list_size_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http_max_header_list_size: 4kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create many headers that together exceed 4KB
    let mut headers = HashMap::new();
    for i in 0..100 {
        headers.insert(
            format!("test-header-{i}"),
            format!("value-{}", "x".repeat(50)),
        );
    }

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .headers(headers)
                .build(),
        )
        .await;

    // Should return 431 Request Header Fields Too Large or 400 Bad Request
    assert!(response.status() == 431 || response.status() == 400);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_http_max_header_list_size_within_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http_max_header_list_size: 8kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create headers that together are less than 8KB
    let mut headers = HashMap::new();
    for i in 0..20 {
        headers.insert(
            format!("test-header-{i}"),
            format!("value-{}", "x".repeat(20)),
        );
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

// Test using legacy limits configuration for comparison
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_legacy_limits_max_headers_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http1_max_request_headers: 5
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    for i in 0..6 {
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

// Test that server.http.max_header_size takes precedence over limits.http_max_header_size
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_server_http_max_header_size_precedence() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            server:
              http:
                max_header_size: 2kb
            limits:
              http_max_header_size: 1kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create a header value between 1KB and 2KB
    // If server.http takes precedence, this should succeed (2KB limit)
    // If limits takes precedence, this should fail (1KB limit)
    let header_value = "x".repeat(1500);

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", header_value)
                .build(),
        )
        .await;

    // Should succeed because server.http.max_header_size (2KB) takes precedence
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}

// Test that server.http.max_headers takes precedence over limits.http1_max_request_headers
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_server_http_max_headers_precedence() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            server:
              http:
                max_headers: 10
            limits:
              http1_max_request_headers: 5
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create 8 headers - should succeed if server.http.max_headers (10) takes precedence
    // Should fail if limits.http1_max_request_headers (5) takes precedence
    let mut headers = HashMap::new();
    for i in 0..8 {
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

    // Should succeed because server.http.max_headers (10) takes precedence
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}

// Test backward compatibility - limits config works when server.http is not set
#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_limits_config_backward_compatibility() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            limits:
              http_max_header_size: 1kb
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Create a header value larger than 1KB
    let large_header_value = "x".repeat(2048);

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query":  "{ __typename }"}))
                .header("test-header", large_header_value)
                .build(),
        )
        .await;

    // Should fail because limits.http_max_header_size (1KB) is enforced
    assert!(response.status() == 431 || response.status() == 400);
    Ok(())
}
