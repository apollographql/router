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
