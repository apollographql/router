use std::path::PathBuf;

use crate::integration::common::graph_os_enabled;
use crate::integration::IntegrationTest;

const PROMETHEUS_METRICS_CONFIG: &str = include_str!("telemetry/fixtures/prometheus.router.yaml");
const LEGACY_QP: &str = "experimental_query_planner_mode: legacy";
const NEW_QP: &str = "experimental_query_planner_mode: new";
const BOTH_QP: &str = "experimental_query_planner_mode: both";
const BOTH_BEST_EFFORT_QP: &str = "experimental_query_planner_mode: both_best_effort";
const NEW_BEST_EFFORT_QP: &str = "experimental_query_planner_mode: new_best_effort";

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_legacy_qp() {
    let mut router = IntegrationTest::builder()
        .config(LEGACY_QP)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config(NEW_QP)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_both_qp() {
    let mut router = IntegrationTest::builder()
        .config(BOTH_QP)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_both_best_effort_qp() {
    let mut router = IntegrationTest::builder()
        .config(BOTH_BEST_EFFORT_QP)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "Falling back to the legacy query planner: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported. \
             Please recompose your supergraph with federation version 2 or greater",
        )
        .await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_new_best_effort_qp() {
    let mut router = IntegrationTest::builder()
        .config(NEW_BEST_EFFORT_QP)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "Falling back to the legacy query planner: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported. \
             Please recompose your supergraph with federation version 2 or greater",
        )
        .await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_legacy_qp_reload_to_new_keep_previous_config() {
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{LEGACY_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{NEW_QP}");
    router.update_config(&config).await;
    router
        .assert_log_contains("error while reloading, continuing with previous configuration")
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_error_kind="fed1",init_is_success="false",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_legacy_qp_reload_to_both_best_effort_keep_previous_config() {
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{LEGACY_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph("../examples/graphql/supergraph-fed1.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{BOTH_BEST_EFFORT_QP}");
    router.update_config(&config).await;
    router
        .assert_log_contains(
            "Falling back to the legacy query planner: \
             failed to initialize the query planner: \
             Supergraphs composed with federation version 1 are not supported. \
             Please recompose your supergraph with federation version 2 or greater",
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_error_kind="fed1",init_is_success="false",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed2_schema_with_new_qp() {
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{NEW_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
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
async fn context_with_legacy_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS_METRICS_CONFIG)
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_with_new_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config(NEW_QP)
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             failed to initialize the query planner: \
             `experimental_query_planner_mode: new` or `both` cannot yet \
             be used with `@context`. \
             Remove uses of `@context` to try the experimental query planner, \
             otherwise switch back to `legacy` or `both_best_effort`.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_with_legacy_qp_change_to_new_qp_keeps_old_config() {
    if !graph_os_enabled() {
        return;
    }
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{LEGACY_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{NEW_QP}");
    router.update_config(&config).await;
    router
        .assert_log_contains("error while reloading, continuing with previous configuration")
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_error_kind="context",init_is_success="false",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_with_legacy_qp_reload_to_both_best_effort_keep_previous_config() {
    if !graph_os_enabled() {
        return;
    }
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{LEGACY_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph("tests/fixtures/set_context/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{BOTH_BEST_EFFORT_QP}");
    router.update_config(&config).await;
    router
        .assert_log_contains(
            "Falling back to the legacy query planner: \
             failed to initialize the query planner: \
             `experimental_query_planner_mode: new` or `both` cannot yet \
             be used with `@context`. \
             Remove uses of `@context` to try the experimental query planner, \
             otherwise switch back to `legacy` or `both_best_effort`.",
        )
        .await;
    router
        .assert_metrics_contains(
            r#"apollo_router_lifecycle_query_planner_init_total{init_error_kind="context",init_is_success="false",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_legacy_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config(LEGACY_QP)
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_new_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config(NEW_QP)
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_both_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config(BOTH_QP)
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_schema_with_both_best_effort_qp_fails_startup() {
    let mut router = IntegrationTest::builder()
        .config(BOTH_BEST_EFFORT_QP)
        .supergraph("tests/fixtures/broken-supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             Federation error: Invalid supergraph: must be a core schema",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_schema_with_new_qp_change_to_broken_schema_keeps_old_config() {
    let config = format!("{PROMETHEUS_METRICS_CONFIG}\n{NEW_QP}");
    let mut router = IntegrationTest::builder()
        .config(config)
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
        .assert_log_contains("error while reloading, continuing with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}
