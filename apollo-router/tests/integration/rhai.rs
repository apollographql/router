use std::path::PathBuf;

use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn all_rhai_callbacks_are_invoked() {
    let config = r#"
rhai:
  scripts: tests/fixtures
  main: test_callbacks.rhai
"#;

    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph(PathBuf::from("tests/fixtures/supergraph.graphql"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute a query to trigger all the callbacks
    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({
                    "query": "{ topProducts { name } }",
                    "variables": {}
                }))
                .build(),
        )
        .await;

    assert!(response.status().is_success());

    // Read all the logs
    router.read_logs();

    for expected_log in [
        "router_service setup",
        "from_router_request",
        "from_router_response",
        "supergraph_service setup",
        "from_supergraph_request",
        "from_supergraph_response",
        "execution_service setup",
        "from_execution_request",
        "from_execution_response",
        "subgraph_service setup",
        "from_subgraph_request",
    ] {
        router.assert_log_contained(expected_log);
    }

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_rhai_hot_reload_works() {
    let (sender, receiver) = tokio::sync::oneshot::channel();

    let mut current_dir = std::env::current_dir().expect("we have a current directory");
    current_dir.push("tests");
    current_dir.push("fixtures");
    let mut test_reload = current_dir.clone();
    let mut test_reload_1 = current_dir.clone();
    let mut test_reload_2 = current_dir.clone();

    test_reload.push("test_reload.rhai");
    test_reload_1.push("test_reload_1.rhai");
    test_reload_2.push("test_reload_2.rhai");

    // Setup our initial rhai file which contains log messages prefixed with 1.
    std::fs::copy(&test_reload_1, &test_reload).expect("could not write rhai test file");

    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/rhai_reload.router.yaml"))
        .collect_stdio(sender)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_query(Query::default()).await;

    // Copy our updated rhai file which contains log messages prefixed with 2.
    std::fs::copy(&test_reload_2, &test_reload).expect("could not write rhai test file");
    // Wait for the router to reload (triggered by our update to the rhai file)
    router.assert_reloaded().await;

    router.execute_query(Query::default()).await;
    router.graceful_shutdown().await;

    let logs = receiver.await.expect("logs received");

    for expected_log in [
        "router_service setup",
        "from_router_request",
        "from_router_response",
        "supergraph_service setup",
        "from_supergraph_request",
        "from_supergraph_response",
        "execution_service setup",
        "from_execution_request",
        "from_execution_response",
        "subgraph_service setup",
        "from_subgraph_request",
    ] {
        // We should see 1. and 2. versions of the expected logs
        for i in 1..3 {
            let expected = format!("{i}. {expected_log}");
            assert!(logs.contains(&expected));
        }
    }
    std::fs::remove_file(&test_reload).expect("could not remove rhai test file");
}
