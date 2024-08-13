use crate::integration::common::graph_os_enabled;
use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_legacy_qp() {
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: legacy")
        .supergraph("../examples/graphql/local.graphql")
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
        .config("experimental_query_planner_mode: new")
        .supergraph("../examples/graphql/local.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             The supergraph schema failed to produce a valid API schema: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_both_qp() {
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: both")
        .supergraph("../examples/graphql/local.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             The supergraph schema failed to produce a valid API schema: \
             Supergraphs composed with federation version 1 are not supported.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed1_schema_with_both_best_effort_qp() {
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: both_best_effort")
        .supergraph("../examples/graphql/local.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "Failed to initialize the new query planner, falling back to legacy: \
             The supergraph schema failed to produce a valid API schema: \
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
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: legacy")
        .supergraph("../examples/graphql/local.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;

    router
        .update_config("experimental_query_planner_mode: new")
        .await;
    router
        .assert_log_contains("error while reloading, continuing with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fed2_schema_with_new_qp() {
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: new")
        .supergraph("../examples/graphql/supergraph-fed2.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_override_with_legacy_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: legacy")
        .supergraph("src/plugins/progressive_override/testdata/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_override_with_new_qp() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: new")
        .supergraph("src/plugins/progressive_override/testdata/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router
        .assert_log_contains(
            "could not create router: \
             The supergraph schema failed to produce a valid API schema: \
             `experimental_query_planner_mode: new` or `both` cannot yet \
             be used with progressive overrides. \
             Remove uses of progressive overrides to try the experimental query planner, \
             otherwise switch back to `legacy` or `both_best_effort`.",
        )
        .await;
    router.assert_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn progressive_override_with_legacy_qp_change_to_new_qp_keeps_old_config() {
    if !graph_os_enabled() {
        return;
    }
    let mut router = IntegrationTest::builder()
        .config("experimental_query_planner_mode: legacy")
        .supergraph("src/plugins/progressive_override/testdata/supergraph.graphql")
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    router
        .update_config("experimental_query_planner_mode: new")
        .await;
    router
        .assert_log_contains("error while reloading, continuing with previous configuration")
        .await;
    router.execute_default_query().await;
    router.graceful_shutdown().await;
}
