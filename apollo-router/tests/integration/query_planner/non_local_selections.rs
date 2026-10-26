use std::collections::HashMap;

use apollo_router::graphql;
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
    // This assertion makes sure that we have all the custom buckets set up
    // specifically for non_local_selections metric.
    //
    // Because Prometheus metrics are cumulative, and this operation falls in
    // the first bucket, all the following buckets will have that operation.
    router
        .assert_metrics_contains_multiple(
            vec![
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="100"} 1"#,
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="1000"} 1"#,
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="25000"} 1"#,
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="50000"} 1"#,
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="100000"} 1"#,
                r#"apollo_router_query_planning_plan_non_local_selections_bucket{<anyopt>le="150000"} 1"#,
            ],
            None,
        )
        .await;

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn max_non_local_selections_env_var_sets_limit() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_ROUTER_MAX_NON_LOCAL_SELECTIONS".to_string(),
        "5".into(),
    );
    let mut router = IntegrationTest::builder()
        .config("{}")
        .supergraph("tests/integration/fixtures/query_planner_max_evaluated_plans.graphql")
        .env(env)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.assert_log_contained(r#""non-local selections limit set","limit":5"#);

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
    let response = response
        .json::<graphql::Response>()
        .await
        .expect("response is a GraphQL response");
    let error = response.errors.first().expect("response has an error");
    assert!(
        error
            .message
            .contains("Number of non-local selections exceeds limit of 5"),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        error.extension_code().as_deref(),
        Some("INTERNAL_SERVER_ERROR")
    );

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn max_non_local_selections_env_var_invalid_value_warns() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_ROUTER_MAX_NON_LOCAL_SELECTIONS".to_string(),
        "200_000".into(),
    );
    let mut router = IntegrationTest::builder()
        .config("{}")
        .supergraph("tests/integration/fixtures/query_planner_max_evaluated_plans.graphql")
        .env(env)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.assert_log_contained(
        r#""falling back to default max non-local selection due to invalid APOLLO_ROUTER_MAX_NON_LOCAL_SELECTIONS value","value":"200_000","default":100000"#,
    );

    router.graceful_shutdown().await;
}
