use apollo_router::services::RouterRequest;
use apollo_router::TestHarness;
use tower::ServiceExt;

// This test will fail if run with the "multi_thread" flavor.
// This is because tracing_test doesn't set a global subscriber, so logs will be dropped
// if we're crossing a thread boundary
#[tokio::test]
async fn all_rhai_callbacks_are_invoked() {
    let env_filter = "apollo_router=info";
    let mock_writer = tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
    let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

    let _guard = tracing::dispatcher::set_default(&subscriber);

    let config = serde_json::json!({
        "rhai": {
            "scripts": "tests/fixtures",
            "main": "test_callbacks.rhai",
        }
    });
    let router = TestHarness::builder()
        .configuration(serde_json::from_value(config).unwrap())
        .schema(include_str!("./fixtures/supergraph.graphql"))
        .build()
        .await
        .unwrap();
    let request = RouterRequest::fake_builder()
        .query("{ topProducts { name } }")
        .build()
        .unwrap();
    let _response = router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    dbg!(_response);
    for expected_log in [
        "router_service setup",
        "from_router_request",
        "from_router_response",
        "query_planner_service setup",
        "from_query_planner_response",
        "from_query_planner_request",
        "execution_service setup",
        "from_execution_request",
        "from_execution_response",
        "subgraph_service setup",
        "from_subgraph_request",
    ] {
        assert!(
            tracing_test::internal::logs_with_scope_contain("apollo_router", expected_log),
            "log not found: {expected_log}"
        );
    }
}
