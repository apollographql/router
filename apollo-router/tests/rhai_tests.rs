use apollo_router::graphql;
use apollo_router::services::supergraph;
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
        .configuration_json(config)
        .unwrap()
        .schema(include_str!("./fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query("{ topProducts { name } }")
        .build()
        .unwrap();
    let _response: graphql::Response = serde_json::from_slice(
        router
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap()
            .to_vec()
            .as_slice(),
    )
    .unwrap();
    dbg!(_response);
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
        assert!(
            tracing_test::internal::logs_with_scope_contain("apollo_router", expected_log),
            "log not found: {expected_log}"
        );
    }
}
