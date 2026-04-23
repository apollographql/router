use std::path::PathBuf;

use insta::assert_json_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

// The default wiremock responder returns:
// {"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}
// which is ~79 bytes. A limit of 50 bytes reliably triggers the error.

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_response_size_limit_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            limits:
                subgraph:
                    all:
                        http_max_response_size: 50b
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(
        response.contains("SUBREQUEST_HTTP_ERROR"),
        "expected SUBREQUEST_HTTP_ERROR in response, got: {response}"
    );
    assert!(
        response.contains("exceeded limit"),
        "expected 'exceeded limit' in response, got: {response}"
    );
    let response_json: serde_json::Value = serde_json::from_str(&response)?;
    assert_json_snapshot!(response_json);

    router
        .assert_metrics_contains(
            r#"apollo_router_limits_subgraph_response_size_exceeded_total"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_response_size_limit_not_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            limits:
                subgraph:
                    all:
                        http_max_response_size: 10kib
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(
        !response.contains("SUBREQUEST_HTTP_ERROR"),
        "expected no errors in response, got: {response}"
    );

    router
        .assert_metrics_does_not_contain(
            "apollo_router_limits_subgraph_response_size_exceeded_total",
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_per_subgraph_response_size_limit_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            limits:
                subgraph:
                    subgraphs:
                        products:
                            http_max_response_size: 50b
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(
        response.contains("SUBREQUEST_HTTP_ERROR"),
        "expected SUBREQUEST_HTTP_ERROR in response, got: {response}"
    );
    let response_json: serde_json::Value = serde_json::from_str(&response)?;
    assert_json_snapshot!(response_json);

    router
        .assert_metrics_contains(
            r#"apollo_router_limits_subgraph_response_size_exceeded_total{subgraph_name="products""#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_response_size_limit_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            connectors:
                sources:
                    connectors.jsonPlaceholder: {}
            limits:
                connector:
                    sources:
                        connectors.jsonPlaceholder:
                            http_max_response_size: 50b
            "#,
        )
        .supergraph(PathBuf::from_iter([
            "..",
            "apollo-router",
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "query ExampleQuery { posts { id } }", "variables": {}}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(
        response.contains("CONNECTOR_RESPONSE_SIZE_LIMIT_EXCEEDED"),
        "expected CONNECTOR_RESPONSE_SIZE_LIMIT_EXCEEDED in response, got: {response}"
    );
    let response_json: serde_json::Value = serde_json::from_str(&response)?;
    assert_json_snapshot!(response_json);

    router
        .assert_metrics_contains(
            r#"apollo_router_limits_connector_response_size_exceeded_total"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connector_response_size_limit_not_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            connectors:
                sources:
                    connectors.jsonPlaceholder: {}
            limits:
                connector:
                    all:
                        http_max_response_size: 10kib
            "#,
        )
        .supergraph(PathBuf::from_iter([
            "..",
            "apollo-router",
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
            "body": "This is a really great post",
            "userId": 1
        }])))
        .http_method("GET")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "query ExampleQuery { posts { id } }", "variables": {}}))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(
        !response.contains("SUBREQUEST_HTTP_ERROR"),
        "expected no errors in response, got: {response}"
    );

    router
        .assert_metrics_does_not_contain(
            "apollo_router_limits_connector_response_size_exceeded_total",
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}
