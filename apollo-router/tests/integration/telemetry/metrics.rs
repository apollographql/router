use std::time::Duration;

use serde_json::json;

use crate::integration::IntegrationTest;

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");
const SUBGRAPH_AUTH_CONFIG: &str = include_str!("fixtures/subgraph_auth.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        router.execute_default_query().await;
        router.execute_default_query().await;
        router.execute_default_query().await;

        // Get Prometheus metrics.
        let metrics_response = router.get_metrics_response().await.unwrap();

        // Validate metric headers.
        let metrics_headers = metrics_response.headers();
        assert!(
            "text/plain; version=0.0.4"
                == metrics_headers
                    .get(http::header::CONTENT_TYPE)
                    .unwrap()
                    .to_str()
                    .unwrap()
        );

        router.touch_config().await;
        router.assert_reloaded().await;
    }

    router.assert_metrics_contains(r#"apollo_router_cache_hit_count_total{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 4"#, None).await;
    router.assert_metrics_contains(r#"apollo_router_cache_miss_count_total{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 2"#, None).await;
    router.assert_metrics_contains(r#"apollo_router_http_request_duration_seconds_bucket{status="200",otel_scope_name="apollo/router",le="100"}"#, None).await;
    router
        .assert_metrics_contains(r#"apollo_router_cache_hit_time"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_cache_miss_time"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_session_count_total"#, None)
        .await;
    router
        .assert_metrics_contains(r#"custom_header="test_custom""#, None)
        .await;
    router
        .assert_metrics_does_not_contain(r#"_total_total{"#)
        .await;

    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        router.assert_metrics_contains(r#"apollo_router_telemetry_studio_reports_total{report_type="metrics",otel_scope_name="apollo/router"} 2"#, Some(Duration::from_secs(10))).await;
        router.assert_metrics_contains(r#"apollo_router_telemetry_studio_reports_total{report_type="traces",otel_scope_name="apollo/router"} 2"#, Some(Duration::from_secs(10))).await;
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="License",url="https://uplink.api.apollographql.com/",otel_scope_name="apollo/router"}"#, Some(Duration::from_secs(120))).await;
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_count_total{query="License",status="success",otel_scope_name="apollo/router"}"#, Some(Duration::from_secs(1))).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_auth_metrics() {
    let mut router = IntegrationTest::builder()
        .config(SUBGRAPH_AUTH_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.execute_default_query().await;

    // Remove auth
    router.update_config(PROMETHEUS_CONFIG).await;
    router.assert_reloaded().await;
    // This one will not be signed, counters shouldn't increment.
    router
        .execute_query(&json! {{ "query": "query { me { name } }"}})
        .await;

    // Get Prometheus metrics.
    let metrics_response = router.get_metrics_response().await.unwrap();

    // Validate metric headers.
    let metrics_headers = metrics_response.headers();
    assert!(
        "text/plain; version=0.0.4"
            == metrics_headers
                .get(http::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
    );

    router.assert_metrics_contains(r#"apollo_router_operations_authentication_aws_sigv4_total{authentication_aws_sigv4_failed="false",subgraph_service_name="products",otel_scope_name="apollo/router"} 2"#, None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_bad_query() {
    let mut router = IntegrationTest::builder()
        .config(SUBGRAPH_AUTH_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    // This query won't make it to the supergraph service
    router.execute_bad_query().await;
    router.assert_metrics_contains(r#"apollo_router_operations_total{http_response_status_code="400",otel_scope_name="apollo/router"} 1"#, None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_bad_queries() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_http_requests_total{status="200",otel_scope_name="apollo/router"}"#,
            None,
        )
        .await;
    router.execute_bad_content_type().await;

    router
            .assert_metrics_contains(
                r#"apollo_router_http_requests_total{error="'content-type' header must be one of: \"application/json\" or \"application/graphql-response+json\"",status="415",otel_scope_name="apollo/router"}"#,
                None,
            )
            .await;

    router.execute_bad_query().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_http_requests_total{error="Must provide query string",status="400",otel_scope_name="apollo/router"}"#,
            None,
        )
        .await;

    router.execute_huge_query().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_http_requests_total{error="payload too large for the `http_max_request_bytes` configuration",status="413",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_log_not_contains(
            "OpenTelemetry metric error occurred: Metrics error: Instrument description conflict",
        )
        .await;
}
