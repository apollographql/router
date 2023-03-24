use std::time::Duration;

use apollo_router::graphql;
use futures::FutureExt;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::common::IntegrationTest;

mod common;

const HAPPY_CONFIG: &str = include_str!("fixtures/jaeger.router.yaml");
const BROKEN_PLUGIN_CONFIG: &str = include_str!("fixtures/broken_plugin.router.yaml");
const INVALID_CONFIG: &str = "garbage: garbage";

#[tokio::test(flavor = "multi_thread")]
async fn test_happy() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_config() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(INVALID_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_not_started().await;
    router.assert_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_valid() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.touch_config().await;
    router.assert_reloaded().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.update_config(BROKEN_PLUGIN_CONFIG).await;
    router.assert_not_reloaded().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_with_broken_plugin_recovery() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .build()
        .await;
    for i in 0..3 {
        println!("iteration {i}");
        router.start().await;
        router.assert_started().await;
        router.run_query().await;
        router.update_config(BROKEN_PLUGIN_CONFIG).await;
        router.assert_not_reloaded().await;
        router.run_query().await;
        router.update_config(HAPPY_CONFIG).await;
        router.assert_reloaded().await;
        router.run_query().await;
        router.graceful_shutdown().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(target_family = "unix")]
async fn test_graceful_shutdown() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(HAPPY_CONFIG)
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ).set_delay(Duration::from_secs(2)))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Send a request in another thread, it'll take 2 seconds to respond, so we can shut down the router while it is in flight.
    let client_handle = tokio::task::spawn(router.run_query().then(|(_, response)| async {
        serde_json::from_slice::<graphql::Response>(&response.bytes().await.unwrap()).unwrap()
    }));

    // Pause to ensure that the request is in flight.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    router.graceful_shutdown().await;

    // We've shut down the router, but we should have got the full response.
    let data = client_handle.await.unwrap();
    insta::assert_json_snapshot!(data);

    Ok(())
}
