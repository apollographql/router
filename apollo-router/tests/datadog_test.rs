mod common;
use tower::BoxError;

use crate::common::IntegrationTest;

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_datadog_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_datadog::new_pipeline()
        .with_service_name("my_app")
        .install_batch(opentelemetry::runtime::Tokio)?;

    let mut router = IntegrationTest::new(
        tracer,
        opentelemetry_datadog::DatadogPropagator::new(),
        include_str!("fixtures/datadog.router.yaml"),
    )
    .await;

    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    Ok(())
}
