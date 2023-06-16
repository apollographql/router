use std::result::Result;
use std::time::Duration;

use apollo_router::_private::create_test_service_factory_from_yaml;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;

mod common;

// This test must use the multi_thread tokio executor or the opentelemetry hang bug will
// be encountered. (See https://github.com/open-telemetry/opentelemetry-rust/issues/536)
#[tokio::test(flavor = "multi_thread")]
#[tracing_test::traced_test]
async fn test_telemetry_doesnt_hang_with_invalid_schema() {
    create_test_service_factory_from_yaml(
        include_str!("../src/testdata/invalid_supergraph.graphql"),
        r#"
    telemetry:
      tracing:
        trace_config:
          service_name: router
        otlp:
          endpoint: default
"#,
    )
    .await;
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp)
        .config(include_str!("fixtures/otlp.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_datadog_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Zipkin)
        .config(include_str!("fixtures/zipkin.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let (_, response) = router.run_query().await;
    assert!(response.headers().get("apollo-trace-id").is_none());

    Ok(())
}

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_CONFIG)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        router.run_query().await;
        router.run_query().await;
        router.run_query().await;

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

    router.assert_metrics_contains(r#"apollo_router_cache_hit_count{kind="query planner",service_name="apollo-router",storage="memory"} 4"#, None).await;
    router.assert_metrics_contains(r#"apollo_router_cache_miss_count{kind="query planner",service_name="apollo-router",storage="memory"} 2"#, None).await;
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
        .assert_metrics_contains(r#"apollo_router_session_count_active"#, None)
        .await;
    router
        .assert_metrics_contains(r#"custom_header="test_custom""#, None)
        .await;

    if std::env::var("APOLLO_KEY").is_ok() && std::env::var("APOLLO_GRAPH_REF").is_ok() {
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="License",service_name="apollo-router",url="https://uplink.api.apollographql.com/",otel_scope_name="apollo/router",otel_scope_version=""}"#, Some(Duration::from_secs(120))).await;
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_count_total{query="License",service_name="apollo-router",status="success",otel_scope_name="apollo/router",otel_scope_version=""}"#, Some(Duration::from_secs(1))).await;
    }

    Ok(())
}
