use std::path::PathBuf;

use tower::BoxError;

use crate::integration::IntegrationTest;

const INCOMPATIBLE_PLUGINS_CONFIG: &str =
    include_str!("../fixtures/connectors/incompatible.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn test_incompatible_plugin_warnings() -> Result<(), BoxError> {
    // Ensure that we have the test keys before running
    // Note: The [IntegrationTest] ensures that these test credentials get
    // set before running the router.
    if std::env::var("TEST_APOLLO_KEY").is_err() || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
    {
        return Ok(());
    };

    let mut router = IntegrationTest::builder()
        .config(INCOMPATIBLE_PLUGINS_CONFIG)
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .build()
        .await;

    router.start().await;

    // Make sure that we have the warnings we expect
    // Note: This order is sadly required since asserting that logs exist consume
    // previous statements...
    let plugins = [
        "apq",
        "authentication",
        "batching",
        "coprocessor",
        "headers",
        "preview_entity_cache",
        "telemetry",
        "tls",
        "traffic_shaping",
        // These must be at the end
        "demand_control",
        "rhai",
    ];
    for plugin in plugins {
        let msg = format!(
            r#""subgraphs":"connectors","message":"plugin `{plugin}` is enabled for connector-enabled subgraphs, which is currently unsupported"#
        );
        router.assert_log_contains(&msg).await;
    }

    Ok(())
}
