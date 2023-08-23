use std::time::Duration;

use serde_json::json;
use tower::BoxError;

use crate::common::IntegrationTest;

mod common;

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");
const SUBGRAPH_AUTH_CONFIG: &str = include_str!("fixtures/subgraph_auth.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() -> Result<(), BoxError> {
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

    router.assert_metrics_contains(r#"apollo_router_cache_hit_count_total{kind="query planner",service_name="apollo-router",storage="memory",otel_scope_name="apollo/router",otel_scope_version=""} 4"#, None).await;
    router.assert_metrics_contains(r#"apollo_router_cache_miss_count_total{kind="query planner",service_name="apollo-router",storage="memory",otel_scope_name="apollo/router",otel_scope_version=""} 2"#, None).await;
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
    router
        .assert_metrics_does_not_contain(r#"_total_total{"#)
        .await;

    if std::env::var("APOLLO_KEY").is_ok() && std::env::var("APOLLO_GRAPH_REF").is_ok() {
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="License",service_name="apollo-router",url="https://uplink.api.apollographql.com/",otel_scope_name="apollo/router",otel_scope_version=""}"#, Some(Duration::from_secs(120))).await;
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_count_total{query="License",service_name="apollo-router",status="success",otel_scope_name="apollo/router",otel_scope_version=""}"#, Some(Duration::from_secs(1))).await;
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_auth_metrics() -> Result<(), BoxError> {
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

    router.assert_metrics_contains(r#"apollo_router_operations_authentication_aws_sigv4_total{authentication_aws_sigv4_failed="false",service_name="apollo-router",subgraph_service_name="products",otel_scope_name="apollo/router",otel_scope_version=""} 2"#, None).await;

    Ok(())
}
