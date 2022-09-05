use apollo_router::_private::create_test_service_factory_from_yaml;

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
