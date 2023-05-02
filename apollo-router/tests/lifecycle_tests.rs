use std::time::Duration;

use apollo_router::graphql;
use futures::FutureExt;
use serde_json::json;
use tokio::process::Command;
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

#[tokio::test(flavor = "multi_thread")]
async fn test_force_hot_reload() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            "experimental_chaos:
                force_hot_reload: 10s",
        )
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    tokio::time::sleep(Duration::from_secs(11)).await;
    router.assert_reloaded().await;
    router.graceful_shutdown().await;
    Ok(())
}

async fn command_output(command: &mut Command) -> String {
    let output = command.output().await.unwrap();
    let success = output.status.success();
    let exit_code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "Success: {success:?}\n\
        Exit code: {exit_code:?}\n\
        stderr:\n\
        {stderr}\n\
        stdout:\n\
        {stdout}"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_config_experimental() {
    insta::assert_snapshot!(
        command_output(
            Command::new(IntegrationTest::router_location())
                .arg("config")
                .arg("experimental")
                .env("RUST_BACKTRACE", "") // Avoid "RUST_BACKTRACE=full detected" log on CI
        )
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_config_preview() {
    insta::assert_snapshot!(
        command_output(
            Command::new(IntegrationTest::router_location())
                .arg("config")
                .arg("preview")
                .env("RUST_BACKTRACE", "") // Avoid "RUST_BACKTRACE=full detected" log on CI
        )
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_experimental_notice() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut router = IntegrationTest::builder()
        .config(
            "
            telemetry:
                experimental_logging:
                    format: json
            ",
        )
        .collect_stdio(tx)
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    router.graceful_shutdown().await;

    insta::assert_snapshot!(rx.await.unwrap());
}
