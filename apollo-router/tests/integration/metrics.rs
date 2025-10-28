#[cfg(all(
    feature = "global-allocator",
    not(feature = "dhat-heap"),
    target_os = "linux"
))]
#[tokio::test(flavor = "multi_thread")]
async fn test_jemalloc_metrics_are_emitted() {
    use super::common::IntegrationTest;

    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/prometheus.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_active"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_allocated"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_mapped"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_metadata"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_resident"#, None)
        .await;
    router
        .assert_metrics_contains(r#"apollo_router_jemalloc_retained"#, None)
        .await;
}
