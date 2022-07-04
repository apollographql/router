mod common;
use std::path::Path;

use tower::BoxError;

use crate::common::TracingTest;

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_datadog_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_datadog::new_pipeline()
        .with_service_name("my_app")
        .install_batch(opentelemetry::runtime::Tokio)?;

    let router = TracingTest::new(
        tracer,
        opentelemetry_datadog::DatadogPropagator::new(),
        Path::new("datadog.router.yaml"),
    );
    router.run_query().await;
    Ok(())
}
