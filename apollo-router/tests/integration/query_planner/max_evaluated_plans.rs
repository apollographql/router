use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

fn assert_evaluated_plans(prom: &str, expected: u64) {
    let line = prom
        .lines()
        .find(|line| line.starts_with("apollo_router_query_planning_plan_evaluated_plans_sum"))
        .expect("apollo.router.query_planning.plan.evaluated_plans metric is missing");
    let (_, num) = line
        .split_once(' ')
        .expect("apollo.router.query_planning.plan.evaluated_plans metric has unexpected shape");
    assert_eq!(num, format!("{expected}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_evaluated_plans() {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
              exporters:
                metrics:
                  prometheus:
                    enabled: true
        "#,
        )
        .supergraph("tests/integration/fixtures/query_planner_max_evaluated_plans.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": r#"{ t { v1 v2 v3 v4 } }"#,
                    "variables": {},
                }))
                .build(),
        )
        .await;

    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .expect("metrics are not text?!");
    assert_evaluated_plans(&metrics, 16);

    router.graceful_shutdown().await;
}

// The `experimental_plans_limit` -> `plans_limit` migration is registered under the 3.x
// migration prefix, so it only takes effect once the router's major version is actually 3 —
// it's a no-op under today's 2.x `UpgradeMode::Minor` config loading. Re-enable once the
// router ships 3.0.
#[ignore = "experimental_plans_limit alias only takes effect once the router is on major version 3"]
#[tokio::test(flavor = "multi_thread")]
async fn does_not_exceed_max_evaluated_plans_legacy() {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
              exporters:
                metrics:
                  prometheus:
                    enabled: true
            supergraph:
              query_planning:
                experimental_plans_limit: 4
        "#,
        )
        .supergraph("tests/integration/fixtures/query_planner_max_evaluated_plans.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": r#"{ t { v1 v2 v3 v4 } }"#,
                    "variables": {},
                }))
                .build(),
        )
        .await;

    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .expect("metrics are not text?!");
    assert_evaluated_plans(&metrics, 4);

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_exceed_max_evaluated_plans() {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
              exporters:
                metrics:
                  prometheus:
                    enabled: true
            supergraph:
              query_planning:
                plans_limit: 4
        "#,
        )
        .supergraph("tests/integration/fixtures/query_planner_max_evaluated_plans.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": r#"{ t { v1 v2 v3 v4 } }"#,
                    "variables": {},
                }))
                .build(),
        )
        .await;

    let metrics = router
        .get_metrics_response()
        .await
        .expect("failed to fetch metrics")
        .text()
        .await
        .expect("metrics are not text?!");
    assert_evaluated_plans(&metrics, 4);

    router.graceful_shutdown().await;
}
