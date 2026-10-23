use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

const QUERY: &str = r#"{ t { v1 v2 v3 v4 } }"#;

#[tokio::test(flavor = "multi_thread")]
async fn reports_non_local_selections() {
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
                    "query": QUERY,
                    "variables": {},
                }))
                .build(),
        )
        .await;

    router
        .assert_metrics_contains(
            r#"apollo_router_query_planning_plan_non_local_selections_sum{<any>} 10"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}
