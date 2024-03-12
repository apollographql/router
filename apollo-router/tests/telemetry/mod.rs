mod metrics;

use crate::common::IntegrationTest;
const PROMETHEUS_CONFIG: &str = include_str!("../fixtures/prometheus.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_request_cancel() {
    let mut router = IntegrationTest::builder()
        .config(include_str!("../fixtures/jaeger.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let res = tokio::time::timeout(
        std::time::Duration::from_micros(100),
        router.execute_default_query(),
    )
    .await;
    println!("res: {res:?}");
    tokio::time::sleep(std::time::Duration::from_millis(10000)).await;

    router
        .assert_log_contains("broken pipe: the client closed the connection")
        .await;
}
