use std::time::Duration;

use insta::assert_yaml_snapshot;
use tower::BoxError;
use wiremock::ResponseTemplate;

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
                    timeout: 10ms
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
                    timeout: 10ms
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
async fn test_router_rate_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(format!(
            r#"
            {PROMETHEUS_CONFIG}
            traffic_shaping:
                router:
                    global_rate_limit:
                        capacity: 1
                        interval: 100ms
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
                        interval: 1min
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
