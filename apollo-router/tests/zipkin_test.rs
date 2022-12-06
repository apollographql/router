mod common;
use std::path::Path;

use tower::BoxError;

use crate::common::TracingTest;
#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_zipkin::new_pipeline()
        .with_service_name("my_app")
        .install_batch(opentelemetry::runtime::Tokio)?;

    let router = TracingTest::new(
        tracer,
        opentelemetry_zipkin::Propagator::new(),
        Path::new("zipkin.router.yaml"),
    );
    let (_, response) = router.run_query().await;
    assert!(response.headers().get("apollo-trace-id").is_none());

    Ok(())
}
