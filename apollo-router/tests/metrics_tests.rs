use tower::BoxError;

use crate::common::IntegrationTest;

mod common;

const PROMETHEUS_CONFIG: &str = include_str!("fixtures/prometheus.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_metrics_reloading() -> Result<(), BoxError> {
    let mut router = create_router(PROMETHEUS_CONFIG).await?;
    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        router.run_query().await;
        router.run_query().await;
        router.run_query().await;

        let metrics = router.get_metrics().await.unwrap();
        assert!(metrics.contains(r#"apollo_router_cache_hit_count{kind="query planner",service_name="apollo-router",storage="memory"} 2"#));
        assert!(metrics.contains(r#"apollo_router_cache_miss_count{kind="query planner",service_name="apollo-router",storage="memory"} 1"#));
        assert!(metrics.contains("apollo_router_cache_hit_time"));
        assert!(metrics.contains("apollo_router_cache_miss_time"));
        assert!(metrics.contains("apollo_router_session_count_total"));
        assert!(metrics.contains("apollo_router_session_count_active"));
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    Ok(())
}

async fn create_router(config: &str) -> Result<IntegrationTest, BoxError> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    Ok(IntegrationTest::new(tracer, opentelemetry_jaeger::Propagator::new(), config).await)
}
