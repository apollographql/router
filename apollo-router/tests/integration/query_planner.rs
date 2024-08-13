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
