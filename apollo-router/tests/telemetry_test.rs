mod common;
mod telemetry;

use std::result::Result;

use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp)
        .config(include_str!("fixtures/otlp.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_datadog_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.execute_default_query().await;
    Ok(())
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_tracing() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Zipkin)
        .config(include_str!("fixtures/zipkin.router.yaml"))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let (_, response) = router.execute_default_query().await;
    assert!(response.headers().get("apollo-trace-id").is_none());

    Ok(())
}
