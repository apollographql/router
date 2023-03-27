use std::time::Duration;

use tower::BoxError;

use crate::common::IntegrationTest;

mod common;

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
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_duration_seconds_count{kind="unchanged",query="Entitlement",service_name="apollo-router",url="https://uplink.api.apollographql.com/graphql"}"#, Some(Duration::from_secs(120))).await;
        router.assert_metrics_contains(r#"apollo_router_uplink_fetch_count_total{query="Entitlement",service_name="apollo-router",status="success"}"#, Some(Duration::from_secs(1))).await;
    }

    Ok(())
}
