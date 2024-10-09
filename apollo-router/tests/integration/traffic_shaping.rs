use std::sync::Arc;
use std::time::Duration;

use insta::assert_yaml_snapshot;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::common::graph_os_enabled;
use crate::integration::common::Telemetry;
use crate::integration::IntegrationTest;

const PROMETHEUS_CONFIG: &str = r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_router_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            traffic_shaping:
                router:
                    timeout: 1ns
            "#
        ))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 504);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_TIMEOUT"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_TIMEOUT",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_timeout() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    timeout: 1ns
            "#
        ))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_TIMEOUT"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_TIMEOUT",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_timeout_operation_name_in_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            traffic_shaping:
                router:
                    timeout: 1ns
            "#,
        )
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(&json!({
            "query": "query UniqueName { topProducts { name } }"
        }))
        .await;
    assert_eq!(response.status(), 504);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_TIMEOUT"));

    router
        .assert_log_contains(r#""otel.name":"query UniqueName""#)
        .await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_timeout_custom_metric() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
                instrumentation:
                    instruments:
                        router:
                            http.server.request.duration:
                                attributes:
                                    # Standard attributes
                                    http.response.status_code: true
                                    graphql.error:
                                        on_graphql_error: true
            traffic_shaping:
                router:
                    timeout: 1ns
            "#
        ))
        .responder(ResponseTemplate::new(500).set_delay(Duration::from_millis(20)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 504);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_TIMEOUT"));

    router.assert_metrics_contains(r#"http_server_request_duration_seconds_count{error_type="Gateway Timeout",graphql_error="true",http_request_method="POST",http_response_status_code="504""#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            traffic_shaping:
                router:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            "#
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(!response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 429);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_RATE_LIMITED",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    global_rate_limit:
                        capacity: 1
                        interval: 10min
            "#,
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(!response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    let (_, response) = router.execute_default_query().await;
    assert_eq!(response.status(), 200);
    let response = response.text().await?;
    assert!(response.contains("REQUEST_RATE_LIMITED"));
    assert_yaml_snapshot!(response);

    router.assert_metrics_contains(r#"apollo_router_graphql_error_total{code="REQUEST_RATE_LIMITED",otel_scope_name="apollo/router"} 1"#, None).await;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_query_deduplication_metrics() -> Result<(), BoxError> {
    let mut router: IntegrationTest = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    deduplicate_query: true
            "#,
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let router = Arc::new(tokio::sync::Mutex::new(router));
    let router_clone = router.clone();
    tokio::task::spawn_local(async move {
        let (_, response) = router_clone.lock().await.execute_default_query().await;
        assert_eq!(response.status(), 200);
    });
    let router_clone = router.clone();
    tokio::task::spawn_local(async move {
        let (_, response) = router_clone.lock().await.execute_default_query().await;
        assert_eq!(response.status(), 200);
    });

    let router_clone = router.clone();
    tokio::task::spawn_local(async move {
        let (_, response) = router_clone.lock().await.execute_default_query().await;
        assert_eq!(response.status(), 200);
    });

    router
        .lock()
        .await
        .assert_metrics_contains(
            r#"apollo_router_deduplicated_queries_total{otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;

    router.lock().await.graceful_shutdown().await;
    Ok(())
}
