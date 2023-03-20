mod common;

use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;
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

    let (_, response) = router.run_query().await;
    assert!(response.headers().get("apollo-trace-id").is_none());

    Ok(())
}
