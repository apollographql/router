use serde_json::json;
use tower::BoxError;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Telemetry;

#[tokio::test(flavor = "multi_thread")]
async fn test_json() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/json.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_text() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Jaeger)
        .config(include_str!("fixtures/text.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    router.execute_query(&query).await;
    router.execute_query(&query).await;
    router.assert_log_contains("trace_id").await;
    router.execute_query(&query).await;
    router.assert_log_contains("span_id").await;
    router.graceful_shutdown().await;
    Ok(())
}
