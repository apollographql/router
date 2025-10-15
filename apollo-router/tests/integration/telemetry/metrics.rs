use std::time::Duration;

use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");
const SUBGRAPH_AUTH_CONFIG: &str = include_str!("fixtures/subgraph_auth.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
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
        router.assert_log_not_contained("OpenTelemetry metric error occurred: Metrics error: metrics provider already shut down");
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
        r#"apollo_router_cache_hit_time_count{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 4"#,
    );
    check_metrics_contains(
        &metrics,
        r#"apollo_router_cache_miss_time_count{kind="query planner",storage="memory",otel_scope_name="apollo/router"} 2"#,
    );
    check_metrics_contains(&metrics, r#"apollo_router_cache_hit_time"#);
    check_metrics_contains(&metrics, r#"apollo_router_cache_miss_time"#);

    router
        .assert_metrics_does_not_contain(r#"_total_total{"#)
        .await;

    router.assert_metrics_contains_multiple(vec![
        r#"apollo_router_telemetry_studio_reports_total{report_type="metrics",otel_scope_name="apollo/router"} 2"#,
        r#"apollo_router_telemetry_studio_reports_total{report_type="traces",otel_scope_name="apollo/router"} 2"#,
        r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="License",url="https://uplink.api.apollographql.com/",otel_scope_name="apollo/router"}"#,
        r#"apollo_router_uplink_fetch_count_total{query="License",status="success",otel_scope_name="apollo/router"}"#
        ], Some(Duration::from_secs(10)))
        .await;
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
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
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
    router.execute_query(Query::default()).await;

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
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(SUBGRAPH_AUTH_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    // This query won't make it to the supergraph service
    router
        .execute_query(Query::default().with_bad_query())
        .await;
    router.assert_metrics_contains(r#"apollo_router_operations_total{http_response_status_code="400",otel_scope_name="apollo/router"} 1"#, None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_bad_queries() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router
        .assert_metrics_contains(
            r#"http_server_request_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .execute_query(
            Query::builder()
                .header("apollo-require-preflight", "true")
                .build()
                .with_bad_content_type(),
        )
        .await;

    router
            .assert_metrics_contains(
                r#"http_server_request_duration_seconds_count{error_type="Unsupported Media Type",http_request_method="POST",status="415",otel_scope_name="apollo/router"} 1"#,
                None,
            )
            .await;

    router
        .execute_query(Query::default().with_bad_query())
        .await;
    router
        .assert_metrics_contains(
            r#"http_server_request_duration_seconds_count{error_type="Bad Request",http_request_method="POST",status="400",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;

    router
        .execute_query(Query::default().with_huge_query())
        .await;
    router
        .assert_metrics_contains(
           r#"http_server_request_duration_seconds_count{error_type="Payload Too Large",http_request_method="POST",status="413",otel_scope_name="apollo/router"} 1"#,
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
    router.print_logs();
    router
        .assert_log_not_contains("this is a bug and should not happen")
        .await;
    router
        .assert_metrics_contains(
            r#"my_custom_router_instrument_total{my_response_body="{\"data\":{\"topProducts\":[{\"name\":\"Table\"},{\"name\":\"Couch\"},{\"name\":\"Chair\"}]}}",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"oplimits_aliases_sum{otel_scope_name="apollo/router"} 0"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"oplimits_root_fields_sum{otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"oplimits_depth_sum{otel_scope_name="apollo/router"} 2"#,
            None,
        )
        .await;
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
    router
        .assert_metrics_contains(r#"apollo_router_compute_jobs_duration_seconds_count{job_outcome="executed_ok",job_type="query_parsing",otel_scope_name="apollo/router"} 1"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_compute_jobs_duration_seconds_count{job_outcome="executed_ok",job_type="query_planning",otel_scope_name="apollo/router"} 1"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_compute_jobs_queue_wait_duration_seconds_count{job_type="query_parsing",otel_scope_name="apollo/router"} 1"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_compute_jobs_execution_duration_seconds_count{job_type="query_planning",otel_scope_name="apollo/router"} 1"#, None)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_gauges_on_reload() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
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
    router.execute_query(Query::introspection()).await;

    // Persisted query
    router
        .execute_query(
            Query::builder().body(json!({"query": "{__typename}", "variables":{}, "extensions": {"persistedQuery":{"version" : 1, "sha256Hash" : "ecf4edb46db40b5132295c0291d62fb65d6759a9eedfa4d5d612dd5ec54a6b38"}}})).build()
        )
        .await;

    router
        .assert_metrics_contains(r#"apollo_router_cache_storage_estimated_size{kind="query planner",type="memory",otel_scope_name="apollo/router"} "#, None)
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

    router
        .assert_metrics_contains(r#"apollo_router_pipelines{config_hash="<any>",schema_id="<any>",otel_scope_name="apollo/router"} 1"#, None)
        .await;

    router
        .assert_metrics_contains(
            r#"apollo_router_compute_jobs_queued{otel_scope_name="apollo/router"} 0"#,
            None,
        )
        .await;

    router
        .assert_metrics_contains(
            r#"apollo_router_compute_jobs_active_jobs{job_type="query_parsing",otel_scope_name="apollo/router"} 0"#,
            None,
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_prom_reset_on_reload() {
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/prometheus.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.execute_default_query().await;

    router
        .assert_metrics_contains(
            r#"http_server_request_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 2"#,
            None,
        )
        .await;

    // This config will NOT reload prometheus as the config did not change
    router
        .update_config(include_str!("fixtures/prometheus.router.yaml"))
        .await;
    router.assert_reloaded().await;
    router
        .assert_metrics_contains(
            r#"http_server_request_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 2"#,
            None,
        )
        .await;

    // This config will force a reload as it changes the prometheus buckets
    router
        .update_config(include_str!("fixtures/prometheus_reload.router.yaml"))
        .await;
    router.assert_reloaded().await;
    router.execute_default_query().await;
    router
        .assert_metrics_contains(
            r#"http_server_request_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_prometheus_metric_rename() {
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/prometheus_metric_rename.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute queries to generate metrics
    router.execute_default_query().await;
    router.execute_default_query().await;

    // Get Prometheus metrics
    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .unwrap();

    // Verify the renamed metric exists with Prometheus transformations
    // custom.http.duration → custom_http_duration_seconds (dots to underscores, unit suffix added)
    check_metrics_contains(&metrics, r#"custom_http_duration_seconds_count"#);
    check_metrics_contains(&metrics, r#"custom_http_duration_seconds_sum"#);
    check_metrics_contains(&metrics, r#"custom_http_duration_seconds_bucket"#);

    // Verify the original metric name does NOT exist
    assert!(
        !metrics.contains(r#"http_server_request_duration_seconds"#),
        "Original metric name should not exist after rename"
    );

    // Verify renamed operations metric
    check_metrics_contains(&metrics, r#"custom_operations_count"#);

    // Verify metric is actually recording data
    check_metrics_contains(
        &metrics,
        r#"custom_http_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 2"#,
    );

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_metric_rename_on_reload() {
    // This test verifies that changing the rename field in a view triggers a proper reload
    // and that the new renamed metric appears correctly
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/prometheus_metric_rename.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    router.execute_default_query().await;
    router.execute_default_query().await;

    // Verify initial renamed metric exists
    router
        .assert_metrics_contains(
            r#"custom_http_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 2"#,
            None,
        )
        .await;

    // Reload with different rename
    router
        .update_config(include_str!(
            "fixtures/prometheus_rename_reload.router.yaml"
        ))
        .await;
    router.assert_reloaded().await;

    // Execute another query after reload
    router.execute_default_query().await;

    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .unwrap();

    // After reload, the new renamed metric should exist
    check_metrics_contains(&metrics, r#"reloaded_http_duration_seconds_count"#);

    // Verify metric is recording data with new name
    check_metrics_contains(
        &metrics,
        r#"reloaded_http_duration_seconds_count{http_request_method="POST",status="200",otel_scope_name="apollo/router"} 1"#,
    );

    // Old renamed metric should not exist (metrics reset on reload)
    assert!(
        !metrics.contains(r#"custom_http_duration_seconds"#),
        "Old renamed metric should not exist after reload with different rename"
    );

    router.graceful_shutdown().await;
}
