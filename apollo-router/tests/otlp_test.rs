mod common;
use std::path::Path;
use std::result::Result;

use opentelemetry::sdk::propagation::TraceContextPropagator;
use tower::BoxError;

use crate::common::TracingTest;

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(opentelemetry_otlp::new_exporter().http())
        .install_batch(opentelemetry::runtime::Tokio)?;

    let router = TracingTest::new(
        tracer,
        TraceContextPropagator::new(),
        Path::new("otlp.router.yaml"),
    );
    router.run_query().await;
    Ok(())
}
