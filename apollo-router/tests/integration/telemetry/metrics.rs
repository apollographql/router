use std::time::Duration;

use serde_json::json;

use crate::integration::common::graph_os_enabled;
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

    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .unwrap();

    check_metrics_contains(
        &metrics,
        r#"apollo_router_cache_hit_count_total{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 4"#,
    );
    check_metrics_contains(
        &metrics,
        r#"apollo_router_cache_miss_count_total{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 2"#,
    );
    check_metrics_contains(
        &metrics,
        r#"apollo_router_http_request_duration_seconds_bucket{status="200",otel_scope_name="apollo/router",le="100"}"#,
    );
    check_metrics_contains(&metrics, r#"apollo_router_cache_hit_time"#);
    check_metrics_contains(&metrics, r#"apollo_router_cache_miss_time"#);
    check_metrics_contains(&metrics, r#"apollo_router_session_count_total"#);
    check_metrics_contains(&metrics, r#"custom_header="test_custom""#);

    router
        .assert_metrics_does_not_contain(r#"_total_total{"#)
        .await;

    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        router.assert_metrics_contains_multiple(vec![
                r#"apollo_router_telemetry_studio_reports_total{report_type="metrics",otel_scope_name="apollo/router"} 2"#,
                r#"apollo_router_telemetry_studio_reports_total{report_type="traces",otel_scope_name="apollo/router"} 2"#,
                r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="License",url="https://uplink.api.apollographql.com/",otel_scope_name="apollo/router"}"#,
                r#"apollo_router_uplink_fetch_count_total{query="License",status="success",otel_scope_name="apollo/router"}"#
            ], Some(Duration::from_secs(10)))
            .await;
    }
}

#[track_caller]
fn check_metrics_contains(metrics: &str, text: &str) {
    assert!(
        metrics.contains(text),
        "'{text}' not detected in metrics\n{metrics}"
    );
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
            r#"apollo_router_http_requests_total{error="Request body payload too large",status="413",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_log_not_contains(
            "OpenTelemetry metric error occurred: Metrics error: Instrument description conflict",
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_graphql_metrics() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/graphql.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router
            .assert_metrics_contains(r#"graphql_field_list_length_sum{graphql_field_name="topProducts",graphql_field_type="Product",graphql_type_name="Query",otel_scope_name="apollo/router"} 3"#, None)
            .await;
    router
            .assert_metrics_contains(r#"graphql_field_list_length_bucket{graphql_field_name="topProducts",graphql_field_type="Product",graphql_type_name="Query",otel_scope_name="apollo/router",le="5"} 1"#, None)
            .await;
    router
            .assert_metrics_contains(r#"graphql_field_execution_total{graphql_field_name="name",graphql_field_type="String",graphql_type_name="Product",otel_scope_name="apollo/router"} 3"#, None)
            .await;
    router
            .assert_metrics_contains(r#"graphql_field_execution_total{graphql_field_name="topProducts",graphql_field_type="Product",graphql_type_name="Query",otel_scope_name="apollo/router"} 1"#, None)
            .await;
    router
            .assert_metrics_contains(r#"custom_counter_total{graphql_field_name="name",graphql_field_type="String",graphql_type_name="Product",otel_scope_name="apollo/router"} 3"#, None)
            .await;
    router
            .assert_metrics_contains(r#"custom_histogram_sum{graphql_field_name="topProducts",graphql_field_type="Product",graphql_type_name="Query",otel_scope_name="apollo/router"} 3"#, None)
            .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_gauges_on_reload() {
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/no-telemetry.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.update_config(PROMETHEUS_CONFIG).await;
    router.assert_reloaded().await;

    // Regular query
    router.execute_default_query().await;

    // Introspection query
    router
        .execute_query(&json!({"query":"{__schema {types {name}}}","variables":{}}))
        .await;

    // Persisted query
    router
        .execute_query(
            &json!({"query": "{__typename}", "variables":{}, "extensions": {"persistedQuery":{"version" : 1, "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"}}})
        )
        .await;

    router
        .assert_metrics_contains(r#"apollo_router_cache_storage_estimated_size{kind="query planner",type="memory",otel_scope_name="apollo/router"} "#, None)
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_query_planning_queued{otel_scope_name="apollo/router"} "#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_v8_heap_total_bytes{otel_scope_name="apollo/router"} "#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_v8_heap_total_bytes{otel_scope_name="apollo/router"} "#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_size{kind="APQ",type="memory",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_size{kind="query planner",type="memory",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_size{kind="introspection",type="memory",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
}
