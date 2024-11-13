use std::collections::HashMap;

use serde_json::json;
use tower::BoxError;

use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_errors_on_http1_max_headers() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            supergraph:
              experimental_http1_max_headers: 100
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
        .execute_query_with_headers(&json!({ "query":  "{ __typename }"}), headers)
        .await;
    assert_eq!(response.status(), 431);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_allow_to_change_http1_max_headers() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            supergraph:
              experimental_http1_max_headers: 200
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
        .execute_query_with_headers(&json!({ "query":  "{ __typename }"}), headers)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_allow_to_change_http1_max_buf_size() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            supergraph:
              experimental_http1_max_buf_size: 1000000
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    headers.insert("test-header".to_string(), "x".repeat(1048576 + 1));

    let (_trace_id, response) = router
        .execute_query_with_headers(&json!({ "query":  "{ __typename }"}), headers)
        .await;
    assert_eq!(response.status(), 431);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_supergraph_errors_on_http1_header_that_does_not_fit_inside_buffer(
) -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            supergraph:
              experimental_http1_max_buf_size: 2000000
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let mut headers = HashMap::new();
    headers.insert("test-header".to_string(), "x".repeat(1048576 + 1));

    let (_trace_id, response) = router
        .execute_query_with_headers(&json!({ "query":  "{ __typename }"}), headers)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await?,
        json!({ "data": { "__typename": "Query" } })
    );
    Ok(())
}
