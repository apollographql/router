use std::path::PathBuf;

use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

mod max_evaluated_plans;

const PROMETHEUS_METRICS_CONFIG: &str = include_str!("telemetry/fixtures/prometheus.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .wait_for_log_message(
            "could not create router: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed2_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        .supergraph("../examples/graphql/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_is_success="true",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_with_new_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_new_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config("{}") // Default config
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .wait_for_log_message(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_schema_with_new_qp_change_to_broken_schema_keeps_old_config() {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        .supergraph("tests/fixtures/valid-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_is_success="true",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router
        .update_schema(&PathBuf::from("tests/fixtures/broken-supergraph.graphql"))
        .await;
    router
        .wait_for_log_message("error while reloading, continuing with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn overloaded_compute_job_pool() {
    let mut router = IntegrationTest::builder()
        .env_entry("APOLLO_ROUTER_COMPUTE_THREADS", "1")
        .env_entry("APOLLO_ROUTER_COMPUTE_QUEUE_CAPACITY_PER_THREAD", "1")
        .config(include_str!("fixtures/happy.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    // Fire off 100 concurrent requests
    let requests = (0..100).map(|i| {
        let mut body = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
        // Replace the query nameAlias with a new query that has an alias based on i
        body["query"] = format!(r#"query ExampleQuery{i} {{topProducts{{name}}}}"#).into();

        router.execute_query(Query::builder().body(body).build())
    });
    let responses = futures::future::join_all(requests).await;

    // Assert that at least one response indicates "overloaded"
    let overloaded_count = responses
        .iter()
        .filter(|response| response.1.status() == 503)
        .count();

    assert!(
        overloaded_count > 0,
        "Expected at least one request to be overloaded, but none were."
    );
    router.graceful_shutdown().await;
}
