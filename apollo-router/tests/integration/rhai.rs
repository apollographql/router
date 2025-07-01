use std::path::PathBuf;

use serde_json::json;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn all_rhai_callbacks_are_invoked() {
    // <<<<<<< HEAD
    //     let (sender, receiver) = tokio::sync::oneshot::channel();
    //     let mut router = IntegrationTest::builder()
    //         .config(include_str!("fixtures/rhai_logging.router.yaml"))
    //         .collect_stdio(sender)
    // ||||||| parent of 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))
    //     let env_filter = "apollo_router=info";
    //     let mock_writer = tracing_test::internal::MockWriter::new(tracing_test::internal::global_buf());
    //     let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

    //     let _guard = tracing::dispatcher::set_default(&subscriber);

    //     let config = serde_json::json!({
    //         "rhai": {
    //             "scripts": "tests/fixtures",
    //             "main": "test_callbacks.rhai",
    //         }
    //     });
    //     let router = TestHarness::builder()
    //         .configuration_json(config)
    //         .unwrap()
    //         .schema(include_str!("../fixtures/supergraph.graphql"))
    //         .build_router()
    //         .await
    //         .unwrap();
    //     let request = supergraph::Request::fake_builder()
    //         .query("{ topProducts { name } }")
    // =======
    let config = r#"
rhai:
  scripts: tests/fixtures
  main: test_callbacks.rhai
"#;

    let mut router = IntegrationTest::builder()
        .config(config)
        .supergraph(PathBuf::from("tests/fixtures/supergraph.graphql"))
        // >>>>>>> 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    // <<<<<<< HEAD
    //     router.execute_query(Query::default()).await;
    //     router.graceful_shutdown().await;

    //     let logs = receiver.await.expect("logs received");
    // ||||||| parent of 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))
    //         .unwrap();
    //     let _response: graphql::Response = serde_json::from_slice(
    //         router
    //             .oneshot(request.try_into().unwrap())
    //             .await
    //             .unwrap()
    //             .next_response()
    //             .await
    //             .unwrap()
    //             .unwrap()
    //             .to_vec()
    //             .as_slice(),
    //     )
    //     .unwrap();
    // =======

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
    // >>>>>>> 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))

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
        // <<<<<<< HEAD
        //         assert!(logs.contains(expected_log));
        // ||||||| parent of 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))
        //         assert!(
        //             tracing_test::internal::logs_with_scope_contain("apollo_router", expected_log),
        //             "log not found: {expected_log}"
        //         );
        // =======
        router.assert_log_contained(expected_log);
        // >>>>>>> 811ea8d58 (fix(coprocessor): improve handling of invalid GraphQL responses with conditional validation (#7731))
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
            let expected = format!("{}. {}", i, expected_log);
            assert!(logs.contains(&expected));
        }
    }
    std::fs::remove_file(&test_reload).expect("could not remove rhai test file");
}
