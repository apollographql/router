use std::collections::HashMap;
use std::ffi::OsString;

use reqwest::StatusCode;
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

#[tokio::test(flavor = "multi_thread")]
async fn exceeding_max_non_local_selections_aborts_planning() {
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
        .env(HashMap::from([(
            "APOLLO_ROUTER_MAX_NON_LOCAL_SELECTIONS".to_string(),
            OsString::from("1"),
        )]))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": QUERY,
                    "variables": {},
                }))
                .build(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    router
        .assert_metrics_contains(
            r#"apollo_router_operations_query_planner_non_local_selections_exceeded_total{<any>} 1"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}
