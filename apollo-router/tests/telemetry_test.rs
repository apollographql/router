mod common;
use std::result::Result;

use apollo_router::_private::create_test_service_factory_from_yaml;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;

// This test must use the multi_thread tokio executor or the opentelemetry hang bug will
// be encountered. (See https://github.com/open-telemetry/opentelemetry-rust/issues/536)
#[tokio::test(flavor = "multi_thread")]
#[tracing_test::traced_test]
async fn test_telemetry_doesnt_hang_with_invalid_schema() {
    create_test_service_factory_from_yaml(
        include_str!("../src/testdata/invalid_supergraph.graphql"),
        r#"
    telemetry:
      tracing:
        trace_config:
          service_name: router
        otlp:
          endpoint: default
"#,
    )
    .await;
}

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
