use crate::integration::IntegrationTest;
use crate::integration::common::Query;

#[tokio::test(flavor = "multi_thread")]
async fn all_rhai_callbacks_are_invoked() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut router = IntegrationTest::builder()
        .config(include_str!("fixtures/rhai_logging.router.yaml"))
        .collect_stdio(sender)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
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
        assert!(logs.contains(expected_log));
    }
}
