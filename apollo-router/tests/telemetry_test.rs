use apollo_router::__create_test_service_factory_from_yaml;

// This test must use the multi_thread tokio executor or the opentelemetry hang bug will
// be encountered. (See https://github.com/open-telemetry/opentelemetry-rust/issues/536)
#[tokio::test(flavor = "multi_thread")]
async fn test_telemetry_doesnt_hang_with_invalid_schema() {
    use apollo_router::subscriber::set_global_subscriber;
    use apollo_router::subscriber::RouterSubscriber;
    use tracing_subscriber::EnvFilter;

    // A global subscriber must be set before we start up the telemetry plugin
    let _ = set_global_subscriber(RouterSubscriber::JsonSubscriber(
        tracing_subscriber::fmt::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .json()
            .finish(),
    ));

    __create_test_service_factory_from_yaml(
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
