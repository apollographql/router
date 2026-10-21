use std::path::PathBuf;

use serde_json::Value;
use serde_json::json;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

mod error_paths;
mod max_evaluated_plans;

const PROMETHEUS_METRICS_CONFIG: &str =
    include_str!("../telemetry/fixtures/prometheus.router.yaml");

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
        .wait_for_log_message("error while reloading, still running with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

/// An unhandled internal error (here, an internal query planner failure) must not have its
/// detail echoed back to the caller: the raw message exposes subgraph/supergraph internals the
/// client has no visibility into and cannot act on, so returning it is an information-disclosure
/// risk. The caller gets a generic message with the status and code preserved, while the detail
/// is logged server-side and still counted by the router's error metrics. See RH-1367.
#[tokio::test(flavor = "multi_thread")]
async fn internal_query_planner_error_is_redacted_from_client_response() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        // A supergraph whose `Book.ui` field carries `@requires(fields: "cover { themable }")`
        // while the client requests `cover(size: "large")`. Planning `ui` produces an internal
        // federation error (`operation must not provide conflicting field arguments for the same
        // name 'cover'`). See RH-1367.
        .supergraph("src/testdata/requires_include_conflict_supergraph.graphql")
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    const REQUIRES_INCLUDE_CONFLICT_QUERY: &str = "query Test($enabled: Boolean = false) { \
        queryAllBooks { cover(size: \"large\") { themable } ui @include(if: $enabled) } }";
    let (_, response) = router
        .execute_query(
            Query::builder()
                .body(json!({ "query": REQUIRES_INCLUDE_CONFLICT_QUERY }))
                .build(),
        )
        .await;
    assert_eq!(response.status(), 500);

    let body: Value = serde_json::from_str(&response.text().await?)?;
    let error = &body["errors"][0];
    assert_eq!(error["message"], "internal server error");
    assert_eq!(error["extensions"]["code"], "INTERNAL_SERVER_ERROR");

    // The raw planner detail must not appear anywhere in the client-facing response, but it must
    // still be logged server-side.
    let serialized = serde_json::to_string(&body)?;
    assert!(
        !serialized.contains("conflicting field arguments"),
        "internal error detail leaked to the client: {serialized}"
    );
    router
        .wait_for_log_message("conflicting field arguments")
        .await;

    router
        .assert_metrics_contains(
            r#"apollo_router_graphql_error_total{code="INTERNAL_SERVER_ERROR",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
    Ok(())
}
