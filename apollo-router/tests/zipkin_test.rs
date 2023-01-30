mod common;

use tower::BoxError;

use crate::common::IntegrationTest;
#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn test_tracing() -> Result<(), BoxError> {
    let tracer = opentelemetry_zipkin::new_pipeline()
        .with_service_name("my_app")
        .install_batch(opentelemetry::runtime::Tokio)?;

    let mut router = IntegrationTest::new(
        tracer,
        opentelemetry_zipkin::Propagator::new(),
        include_str!("fixtures/zipkin.router.yaml"),
    )
    .await;
    router.start().await;
    router.assert_started().await;

    let (_, response) = router.run_query().await;
    assert!(response.headers().get("apollo-trace-id").is_none());

    Ok(())
}
