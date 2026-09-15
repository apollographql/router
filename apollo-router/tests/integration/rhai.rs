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

// A script that fails used to report the Rhai error verbatim to the client, disclosing that the
// router runs Rhai, the name of the failing callback, and where in the script it failed.
//
// `script_disclosures` are the details out of that particular script that the old message carried,
// on top of the Rhai internals every failure used to carry. Only pass a string the pre-fix message
// actually contained - anything else asserts nothing, since it was never there to leak.
async fn assert_client_error_omits_rhai_internals(script: &str, script_disclosures: &[&str]) {
    let config = format!(
        r#"
rhai:
  scripts: tests/fixtures
  main: {script}
"#
    );

    let mut router = IntegrationTest::builder()
        .config(config.as_str())
        .supergraph(PathBuf::from("tests/fixtures/supergraph.graphql"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let (_trace_id, response) = router
        .execute_query(
            Query::builder()
                .body(json!({"query": "{ topProducts { name } }", "variables": {}}))
                .build(),
        )
        .await;

    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body = response.text().await.expect("a response body");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).expect("a GraphQL response")["errors"][0]
            ["message"],
        json!("Internal Server Error")
    );
    for disclosure in ["rhai", "Rhai", "Runtime error", "line ", "position "]
        .iter()
        .chain(script_disclosures)
    {
        assert!(
            !body.contains(*disclosure),
            "client response leaks {disclosure:?}: {body}"
        );
    }

    // The reason the client no longer sees has to be in the router's logs instead, otherwise this
    // is just a silent failure.
    router.wait_for_log_message("rhai execution error").await;

    router.graceful_shutdown().await;
}

// A router Rhai function that fails without a message of its own.
#[tokio::test(flavor = "multi_thread")]
async fn client_errors_omit_rhai_internals() {
    // No script-specific disclosure to check: the binding raises an empty message, so the pre-fix
    // client message was `rhai execution error: 'Runtime error (line N, position M)'` and named
    // neither the header nor the callback. The Rhai internals above are all this script leaked.
    assert_client_error_omits_rhai_internals("rhai_redacted_error.rhai", &[]).await;
}

// The Rhai engine's own failure, which never reaches a router function. This needs a script of its
// own: the router_service in rhai_redacted_error.rhai breaks the pipeline before an execution
// callback could run.
#[tokio::test(flavor = "multi_thread")]
async fn client_errors_omit_rhai_engine_internals() {
    // The engine names the function it could not find, so the pre-fix message carried it.
    assert_client_error_omits_rhai_internals(
        "rhai_engine_error.rhai",
        &["this_function_does_not_exist"],
    )
    .await;
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
