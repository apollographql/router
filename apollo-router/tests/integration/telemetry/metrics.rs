use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use opentelemetry_proto::tonic::metrics::v1::metric;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use prost::Message;
use regex::Regex;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;
use crate::integration::telemetry::otlp::mock_otlp_server;

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");
const PROMETHEUS_RESPONSE_BODY_SIZE_CONFIG: &str =
    include_str!("fixtures/prometheus_response_body_size.router.yaml");
const SUBGRAPH_AUTH_CONFIG: &str = include_str!("fixtures/subgraph_auth.router.yaml");
const RESPONSE_CACHE_CONFIG: &str = include_str!("fixtures/response_cache.router.yaml");

/// Prometheus, a connector pointed at the local wiremock, and subgraph errors
/// included so declared errors are not redacted away.
const CONNECTOR_DECLARED_ERROR_CONFIG: &str = r#"
telemetry:
  exporters:
    metrics:
      prometheus:
        enabled: true
        path: /metrics
include_subgraph_errors:
  all: true
connectors:
  sources: {}
"#;

/// The same without `include_subgraph_errors`, which is the default and
/// redacts a connector's declared errors out of the response.
const CONNECTOR_REDACTED_CONFIG: &str = r#"
telemetry:
  exporters:
    metrics:
      prometheus:
        enabled: true
        path: /metrics
connectors:
  sources: {}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() {
    // Force every request to be sampled by Apollo's field-level
    // instrumentation so the apollo_exporter pipeline always sees traces
    // and emits the `studio_reports_total{report_type="traces"}`
    // counter — the production default is 1 % which is non-deterministic
    // over the 6 queries this test sends. Drop the batch processor's
    // `scheduled_delay` so the 30 s assertion deadline doesn't have to
    // wait for a long timer to fire. We patch `PROMETHEUS_CONFIG` in
    // place rather than forking a whole new fixture; `merge_overrides`
    // will subsequently populate the mock endpoints into the same
    // `telemetry.apollo` block.
    //
    // The OTLP traces path defaults to gRPC. wiremock only speaks HTTP,
    // so we pin both OTLP protocols to HTTP/protobuf; otherwise the
    // OTLP traces export silently fails to connect, the
    // `studio_reports_total{report_type="traces"}` counter never
    // increments, and the assertion times out. Apollo-protocol metrics
    // (counter `report_type="metrics"`) use plain HTTP irrespective of
    // this setting.
    let mut config_value: serde_yaml::Value =
        serde_yaml::from_str(PROMETHEUS_CONFIG).expect("fixture is valid YAML");
    let apollo_block: serde_yaml::Value = serde_yaml::from_str(
        "field_level_instrumentation_sampler: always_on\nexperimental_otlp_tracing_protocol: http\nexperimental_otlp_metrics_protocol: http\nbatch_processor:\n  scheduled_delay: 100ms\n",
    )
    .unwrap();
    config_value
        .as_mapping_mut()
        .and_then(|m| m.get_mut(&serde_yaml::Value::String("telemetry".into())))
        .and_then(|t| t.as_mapping_mut())
        .expect("PROMETHEUS_CONFIG has a telemetry block")
        .insert(serde_yaml::Value::String("apollo".into()), apollo_block);
    let config = serde_yaml::to_string(&config_value).unwrap();

    // No explicit env / no real-creds opt-in. The harness now provides:
    //   * Fake `APOLLO_KEY` / `APOLLO_GRAPH_REF` so the executable picks
    //     `LicenseSource::Registry` (and the License poller actually
    //     runs, incrementing
    //     `apollo_router_uplink_fetch_*{query="License"}`).
    //   * `APOLLO_UPLINK_ENDPOINTS` pinned to a per-test wiremock that
    //     responds to `LicenseQuery` with a `RouterEntitlementsResult`
    //     whose `entitlement.jwt` is `TEST_LICENSE_JWT_FULL_FEATURES` —
    //     a real HS256-signed JWT carrying no `allowedFeatures` claim
    //     (legacy-compat → all features allowed).
    //   * `APOLLO_TEST_INTERNAL_UPLINK_JWKS=TEST_JWKS_ENDPOINT` so the
    //     spawned router's `License::jwks()` validates that JWT against
    //     the bundled test JWKS instead of the production JWKS baked
    //     into the binary. The License state machine therefore reaches
    //     `Licensed` (with all features), not the
    //     `Unlicensed` `License::default()` — important for any test
    //     downstream of this harness that exercises a paid feature, but
    //     also fine for this test, which just asserts the License
    //     poller's success counter increments.
    //   * Both `telemetry.apollo.endpoint` and
    //     `telemetry.apollo.experimental_otlp_endpoint` pinned to the
    //     per-test `apollo_otlp_server` mock with a catch-all
    //     `POST → 200` route, so all four reporting paths
    //     (`studio_reports_total{report_type=metrics|traces}` ×
    //     Apollo-protocol + OTLP) complete deterministically.
    //
    // See `apollo-router/tests/common.rs::start()` and
    // `merge_overrides()` for the wiring.
    let mut router = IntegrationTest::builder().config(config).build().await;

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

    // Studio + Uplink metrics. With both Apollo Studio and Apollo Uplink
    // pointed at local wiremocks (Studio via the harness's per-test mock at
    // `experimental_otlp_endpoint`, Uplink via `APOLLO_UPLINK_ENDPOINTS`), all
    // four reporting paths complete successfully and deterministically — no
    // public-Internet dependency, no batch-timer race.
    //
    // Patterns match label fragments rather than full lines because
    // Prometheus orders labels alphabetically and the label set on these
    // metrics grows over time (eg. `report_protocol`,
    // `report_extended_references_enabled` were added to studio_reports).
    // `assert_metrics_contains_multiple` substring-matches anywhere on a
    // line and supports `<any>` (→ `.+`) wildcards, so naming the labels we
    // care about is sufficient.
    // Patterns are substring matches anywhere on a line. We use `<anyopt>`
    // (→ `.*`, zero-or-more) rather than `<any>` (→ `.+`, one-or-more)
    // between the opening `{` and the label we care about, because
    // Prometheus orders labels alphabetically — `query="License"` happens
    // to be the alphabetically-first label on `uplink_fetch_count_total`,
    // so requiring even a single character before it would never match.
    router
        .assert_metrics_contains_multiple(
            vec![
                r#"apollo_router_telemetry_studio_reports_total{<anyopt>report_type="metrics""#,
                r#"apollo_router_telemetry_studio_reports_total{<anyopt>report_type="traces""#,
                r#"apollo_router_uplink_fetch_count_total{<anyopt>query="License"<anyopt>status="success""#,
                r#"apollo_router_uplink_fetch_duration_seconds_count{<anyopt>query="License""#,
            ],
            Some(Duration::from_secs(30)),
        )
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
async fn test_response_cache_metrics() {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(RESPONSE_CACHE_CONFIG)
        .supergraph(PathBuf::from("./testing_schema.graphql"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    let json_query = json!({"query":"{ topProducts { name reviews { body } } }","variables":{}});
    let query = Query::builder()
        // .header("apollo-cache-debugging", "true")
        .body(json_query.clone())
        .build();
    let (_, _resp) = router.execute_query(query.clone()).await;
    router.execute_query(query.clone()).await;
    // To make sure to update the TTL and it's not reflected in metrics
    tokio::time::sleep(Duration::from_millis(1000)).await;
    router.execute_query(query.clone()).await;

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
    let metrics = metrics_response.text().await.unwrap();

    check_metrics_contains(&metrics, r#"cache_control_scope="public""#);
    let regexp = Regex::new(r#"cache_control_max_age="([0-9]+)""#).unwrap();
    let captures: BTreeSet<&str> = regexp.find_iter(&metrics).map(|m| m.as_str()).collect();
    // Checking they all have the same values to avoid computed max age
    assert_eq!(captures.len(), 2);
    let mut captures_iter = captures.iter();
    assert_eq!(
        captures_iter.next().unwrap(),
        &"cache_control_max_age=\"10\""
    );
    assert_eq!(
        captures_iter.next().unwrap(),
        &"cache_control_max_age=\"60\""
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

// Regression test: SigV4 signing params must not leak from a configured subgraph
// to an unconfigured one when both are resolved in the same operation.
#[tokio::test(flavor = "multi_thread")]
async fn test_sigv4_does_not_leak_to_unconfigured_subgraph() {
    if !graph_os_enabled() {
        return;
    }

    // Set up a single mock server that both subgraphs route to.
    let mock_server = MockServer::start().await;
    let subgraph_url = mock_server.uri();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "topProducts": [{"name": "Table"}], "me": {"name": "Alice"} }
        })))
        .mount(&mock_server)
        .await;

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
            authentication:
              subgraph:
                subgraphs:
                  products:
                    aws_sig_v4:
                      hardcoded:
                        access_key_id: "test"
                        secret_access_key: "test"
                        region: "us-east-1"
                        service_name: "test_service"
            "#,
        )
        .subgraph_overrides(HashMap::from([
            ("products".to_string(), subgraph_url.clone()),
            ("accounts".to_string(), subgraph_url.clone()),
        ]))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Query resolves topProducts (products subgraph, SigV4 configured) and me
    // (accounts subgraph, not configured) in the same operation.
    router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ topProducts { name } me { name } }"}))
                .build(),
        )
        .await;

    // Wait for the products signing metric to appear, proving metrics have been
    // flushed, then immediately assert accounts was not signed.
    router
        .assert_metrics_contains(
            r#"apollo_router_operations_authentication_aws_sigv4_total{authentication_aws_sigv4_failed="false",subgraph_service_name="products",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_does_not_contain(
            r#"apollo_router_operations_authentication_aws_sigv4_total{authentication_aws_sigv4_failed="false",subgraph_service_name="accounts""#,
        )
        .await;

    router.graceful_shutdown().await;
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

/// Verifies that when a client sends `Accept-Encoding: gzip`, the
/// `http.server.response.body.size` histogram records the compressed byte count
/// rather than the uncompressed body size.
#[tokio::test(flavor = "multi_thread")]
async fn test_response_body_size_records_compressed_size_with_gzip() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_RESPONSE_BODY_SIZE_CONFIG)
        .reqwest_client(reqwest::Client::builder().gzip(false).build().unwrap())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = Query::builder()
        .body(json!({"query":"{ topProducts { name reviews { id body author { name } } } }"}))
        .header("accept-encoding", "gzip")
        .build();

    let (_, response) = router.execute_query(query).await;
    let content_encoding = response.headers().get("content-encoding").unwrap().to_str();
    assert_eq!(content_encoding.unwrap(), "gzip");

    let response_body_size = response.bytes().await.unwrap().len();
    assert_eq!(response_body_size, 103);

    router
        .assert_metrics_contains(
            r#"http_server_response_body_size_bytes_count{<any>} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"http_server_response_body_size_bytes_sum{<any>} 103"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request_duration_selector() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/request_duration.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router
        .assert_metrics_contains(
            r#"request_happened_total{otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            r#"reasonably_short_total{otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router
        .assert_metrics_does_not_contain(r#"overly_short_total{otel_scope_name="apollo/router"}"#)
        .await;

    router.graceful_shutdown().await;
}

/// Drives an `@defer` query against a router configured with two supergraph
/// counters that split on `is_primary_response`. Returns the scraped
/// Prometheus metrics text so the caller can assert on a specific series.
///
/// Shared by the two regression tests for the selector fix (see PR #9238).
async fn run_is_primary_response_query() -> String {
    use tokio_stream::StreamExt;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    // Products subgraph: returns the primary chunk's data.
    let mock_products = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "topProducts": [
                    {"__typename": "Product", "upc": "1", "name": "Table"},
                    {"__typename": "Product", "upc": "2", "name": "Chair"},
                ]
            }
        })))
        .mount(&mock_products)
        .await;

    // Reviews subgraph: slow so the router defers reviews into a second chunk.
    let mock_reviews = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({
                    "data": {
                        "_entities": [
                            {"reviews": [{"body": "great"}]},
                            {"reviews": [{"body": "ok"}]},
                        ]
                    }
                })),
        )
        .mount(&mock_reviews)
        .await;

    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/is_primary_response.router.yaml"))
        .subgraph_override("products", mock_products.uri())
        .subgraph_override("reviews", mock_reviews.uri())
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let query = Query::builder()
        .body(json!({
            "query": "query Q { topProducts { name ... @defer { reviews { body } } } }"
        }))
        .header("Accept", "multipart/mixed;deferSpec=20220824")
        .build();
    let (_, response) = router.execute_query(query).await;
    assert_eq!(response.status(), 200);

    // Drain the multipart stream so every chunk flows through the
    // `response_stream.inspect(...)` closure and updates metrics.
    let mut stream = response.bytes_stream();
    let mut chunks = 0;
    while let Some(Ok(_)) = stream.next().await {
        chunks += 1;
    }
    assert!(
        chunks >= 2,
        "expected a multipart @defer response with at least 2 chunks, got {chunks}"
    );

    router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .unwrap()
}

/// Regression test for the `is_primary_response` supergraph telemetry selector.
///
/// Before the PR #9238 fix, the selector always evaluated to `false` at
/// `on_response` / `on_response_event` scope because `FIRST_EVENT_CONTEXT_KEY`
/// was never set to `Bool(true)` on the primary chunk. A counter conditioned on
/// `is_primary_response == true` therefore never fired, even for the primary
/// chunk of a multipart `@defer` response.
#[tokio::test(flavor = "multi_thread")]
async fn test_is_primary_response_fires_on_primary_chunk() {
    let metrics = run_is_primary_response_query().await;
    check_metrics_contains(
        &metrics,
        r#"is_primary_chunks_total{otel_scope_name="apollo/router"} 1"#,
    );
}

/// Mirror of `test_is_primary_response_fires_on_primary_chunk` that asserts
/// the selector evaluates to `false` for deferred chunks. The exact count
/// of deferred chunks is a query-planner property — for a defer fragment
/// over multiple entities, the planner may emit one chunk per entity or
/// one chunk total depending on dependency-graph reduction. We only assert
/// the counter fires at least once, since OpenTelemetry counters that never
/// increment are not present in the scrape output at all.
#[tokio::test(flavor = "multi_thread")]
async fn test_is_primary_response_fires_on_deferred_chunks() {
    let metrics = run_is_primary_response_query().await;
    check_metrics_contains(
        &metrics,
        r#"deferred_chunks_total{otel_scope_name="apollo/router"}"#,
    );
}

/// A connector mapping's `->withError` reaches the client in
/// `extensions.connectorErrors` and increments `apollo.router.graphql_error`
/// under the author's code.
///
/// Prometheus rather than the Apollo pipeline so this runs without GraphOS
/// credentials. `apollo.router.operations.error` is denied on public
/// exporters, and its attributes are covered by the `error_counter` unit
/// tests.
///
/// `include_subgraph_errors: all: true` is not decoration: the default redacts
/// subgraph errors and takes declared errors with them. `connectors: sources:
/// {}` points the connector at the local wiremock rather than the schema's
/// real baseURL.
#[tokio::test(flavor = "multi_thread")]
async fn test_declared_connector_error_is_reported_and_counted() {
    let mut router = IntegrationTest::builder()
        .config(CONNECTOR_DECLARED_ERROR_CONFIG)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "with_error.graphql",
        ]))
        // No `body`, so the mapping's `body ?? $("")->withError(...)` declares
        // its error and the field still resolves.
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
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
                .body(json!({"query":"query ExampleQuery {posts{id body}}","variables":{}}))
                .build(),
        )
        .await;

    // The data is returned *and* the defect is reported, alongside each other.
    let response = response.text().await.unwrap();
    assert!(
        response.contains("connectorErrors"),
        "expected the declared error in extensions, got: {response}"
    );
    assert!(
        response.contains("POST_BODY_MISSING"),
        "expected the author's error code, got: {response}"
    );

    router
        .assert_metrics_contains(
            r#"apollo_router_graphql_error_total{code="POST_BODY_MISSING"<any>} 1"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}

/// Counted at the connector, so redaction cannot suppress the metric.
///
/// The two halves are decided in different places: `include_subgraph_errors`
/// governs what a client receives, telemetry config governs what is counted.
/// Without `include_subgraph_errors` (the default, asserted here by its
/// absence) an operator would otherwise have no way to know these happened.
#[tokio::test(flavor = "multi_thread")]
async fn test_declared_connector_error_is_counted_even_when_redacted() {
    let mut router = IntegrationTest::builder()
        .config(CONNECTOR_REDACTED_CONFIG)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "with_error.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "title": "Awesome post",
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
                .body(json!({"query":"query ExampleQuery {posts{id body}}","variables":{}}))
                .build(),
        )
        .await;

    // Redacted out of the response entirely, not replaced by a placeholder.
    let response = response.text().await.unwrap();
    assert!(
        !response.contains("POST_BODY_MISSING"),
        "a redacted subgraph's declared errors must not reach the client, got: {response}"
    );

    // And counted anyway.
    router
        .assert_metrics_contains(
            r#"apollo_router_graphql_error_total{code="POST_BODY_MISSING"<any>} 1"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}

/// `->withWarning` reaches neither the client nor the error counters.
///
/// The negative case is the one that can regress invisibly: a warning that
/// started counting as an error would break the promise the method exists for
/// without breaking any assertion about `->withError`.
///
/// The responder omits `title` and supplies `body`, so the fixture's
/// `->withWarning` fires and its `->withError` does not.
#[tokio::test(flavor = "multi_thread")]
async fn test_connector_mapping_warning_is_not_reported_or_counted() {
    let mut router = IntegrationTest::builder()
        .config(CONNECTOR_DECLARED_ERROR_CONFIG)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "with_error.graphql",
        ]))
        .responder(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
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
                .body(json!({"query":"query ExampleQuery {posts{id title}}","variables":{}}))
                .build(),
        )
        .await;

    let response = response.text().await.unwrap();
    assert!(
        !response.contains("connectorErrors"),
        "a warning must not be reported to the client, got: {response}"
    );
    assert!(
        !response.contains("Post title was missing"),
        "a warning's message must not reach the client, got: {response}"
    );

    // The whole counter, not just this code: the query succeeds with no
    // errors of any kind, so any increment at all is a regression. Asserted
    // after scraping an unrelated counter, so this is "absent once metrics
    // were flowing" rather than "nothing had arrived yet".
    router
        .assert_metrics_contains(r#"apollo_router_operations_total"#, None)
        .await;
    router
        .assert_metrics_does_not_contain(r#"apollo_router_graphql_error_total"#)
        .await;

    router.graceful_shutdown().await;
}

fn cardinality_overflow_lines(metrics: &str) -> Vec<&str> {
    metrics
        .lines()
        .filter(|l| l.starts_with("apollo_router_telemetry_metrics_cardinality_overflow"))
        .collect()
}

/// Sends a successful and a failed request, which is enough to push
/// `http.server.request.duration` past the fixtures' `cardinality_limit: 1`.
async fn overflow_request_duration(router: &mut IntegrationTest) {
    router.execute_default_query().await;
    router
        .execute_query(Query::default().with_bad_query())
        .await;
}

/// The value of the single `cardinality_overflow` series in a scrape, which must be labelled
/// `metric_name`.
async fn scraped_overflow_count(router: &IntegrationTest, metric_name: &str) -> u64 {
    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .unwrap();
    let counter = cardinality_overflow_lines(&metrics);
    assert_eq!(
        counter.len(),
        1,
        "expected one counter series in:\n{metrics}"
    );
    let (series, value) = counter[0].rsplit_once(' ').unwrap();
    assert!(
        series.contains(&format!(r#"metric_name="{metric_name}""#)),
        "{series}"
    );
    value.parse().unwrap()
}

/// With only the Prometheus exporter, an overflowed instrument is reported by
/// `apollo.router.telemetry.metrics.cardinality_overflow`, labelled with its OpenTelemetry name,
/// as with the other exporters. The start of the overflow counts once, however many times the
/// endpoint is scraped.
#[tokio::test(flavor = "multi_thread")]
async fn test_prometheus_cardinality_overflow_counter() {
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/prometheus_cardinality_overflow.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    overflow_request_duration(&mut router).await;

    router
        .assert_metrics_contains_multiple(
            vec![
                r#"http_server_request_duration_seconds_count{otel_metric_overflow="true""#,
                r#"apollo_router_telemetry_metrics_cardinality_overflow_total{metric_name="http.server.request.duration",otel_scope_name="apollo/router"} 1"#,
            ],
            None,
        )
        .await;
    // More scrapes, including from a second client, don't count the same overflow again.
    let second_scraper = reqwest::Client::new();
    for _ in 0..3 {
        second_scraper
            .get(format!("http://{}/metrics", router.bind_address()))
            .send()
            .await
            .expect("second scraper failed");
        assert_eq!(
            scraped_overflow_count(&router, "http.server.request.duration").await,
            1
        );
    }

    router.graceful_shutdown().await;
}

async fn otlp_metric_exports(collector: &MockServer) -> Vec<ExportMetricsServiceRequest> {
    collector
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/metrics")
        .map(|request| {
            ExportMetricsServiceRequest::decode(request.body.as_slice())
                .expect("OTLP metrics export")
        })
        .collect()
}

async fn wait_for_otlp_metric_exports(collector: &MockServer, exports: usize) {
    let target = otlp_metric_exports(collector).await.len() + exports;
    tokio::time::timeout(Duration::from_secs(15), async {
        while otlp_metric_exports(collector).await.len() < target {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("OTLP exporter did not export");
}

/// The value of the `cardinality_overflow` series for `http.server.request.duration` in the most
/// recent OTLP export that carries it. With cumulative temporality that is its running total.
async fn exported_overflow_count(collector: &MockServer) -> Option<i64> {
    otlp_metric_exports(collector)
        .await
        .iter()
        .flat_map(|export| &export.resource_metrics)
        .flat_map(|resource| &resource.scope_metrics)
        .flat_map(|scope| &scope.metrics)
        .filter(|metric| metric.name == "apollo.router.telemetry.metrics.cardinality_overflow")
        .filter_map(|metric| match &metric.data {
            Some(metric::Data::Sum(sum)) => Some(&sum.data_points),
            _ => None,
        })
        .flatten()
        .filter(|point| {
            point.attributes.iter().any(|attribute| {
                attribute.key == "metric.name"
                    && attribute
                        .value
                        .as_ref()
                        .and_then(|value| value.value.as_ref())
                        == Some(&any_value::Value::StringValue(
                            "http.server.request.duration".to_string(),
                        ))
            })
        })
        .filter_map(|point| match point.value {
            Some(number_data_point::Value::AsInt(value)) => Some(value),
            _ => None,
        })
        .next_back()
}

/// With OTLP, alone or with Prometheus, the OTLP exporter counts the start of an overflow once,
/// with the dotted metric name, however many exports follow.
async fn assert_counted_once_by_otlp(collector: &MockServer) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while exported_overflow_count(collector).await.is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("OTLP never exported the overflow counter");
    wait_for_otlp_metric_exports(collector, 3).await;
    assert_eq!(exported_overflow_count(collector).await, Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_cardinality_overflow_counted_once() {
    let collector = mock_otlp_server(0..).await;
    let config = include_str!("fixtures/prometheus_otlp_cardinality_overflow.router.yaml")
        .replace("<otel-collector-endpoint>", &collector.uri())
        .replace(
            "      prometheus:\n        enabled: true\n        path: /metrics\n",
            "",
        );
    // If the fixture changes so the replacement misses, this would silently test both exporters.
    assert!(!config.contains("prometheus:"), "{config}");
    let mut router = IntegrationTest::builder().config(&config).build().await;

    router.start().await;
    router.assert_started().await;
    overflow_request_duration(&mut router).await;
    assert_counted_once_by_otlp(&collector).await;

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_prometheus_and_otlp_cardinality_overflow_counted_once() {
    let collector = mock_otlp_server(0..).await;
    let config = include_str!("fixtures/prometheus_otlp_cardinality_overflow.router.yaml")
        .replace("<otel-collector-endpoint>", &collector.uri());
    let mut router = IntegrationTest::builder().config(&config).build().await;

    router.start().await;
    router.assert_started().await;
    overflow_request_duration(&mut router).await;
    assert_counted_once_by_otlp(&collector).await;
    // Scrapes don't count as well: the endpoint shows OTLP's single dotted series.
    for _ in 0..3 {
        assert_eq!(
            scraped_overflow_count(&router, "http.server.request.duration").await,
            1
        );
    }

    router.graceful_shutdown().await;
}
