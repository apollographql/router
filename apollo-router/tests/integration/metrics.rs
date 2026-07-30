#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
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

/// `apollo.router.query_planner.memory` is recorded from inside the compute job that runs the
/// work, labelled with the job type as `context`. Getting there depends on the planning task
/// being memory-tracked on the async side: `compute_job::execute` snapshots
/// `allocator::current()` when it submits the job, and the worker thread re-parents that snapshot.
/// PR #8902 fixed a regression where the tracking was only applied when cooperative cancellation
/// was enabled, which silently dropped the `context="query_planning"` series.
///
/// Patterns match label fragments rather than full lines because Prometheus orders labels
/// alphabetically and appends a unit suffix to the metric name.
#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
#[tokio::test(flavor = "multi_thread")]
async fn test_query_planner_memory_metrics_are_emitted_without_cooperative_cancellation() {
    use super::common::IntegrationTest;

    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/prometheus_no_cooperative_cancellation.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    router
        .assert_metrics_contains_multiple(
            vec![
                r#"apollo_router_query_planner_memory<any>context="query_planning""#,
                r#"apollo_router_query_planner_memory<any>context="query_parsing""#,
            ],
            None,
        )
        .await;
}
