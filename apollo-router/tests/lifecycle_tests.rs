use tower::BoxError;

use crate::common::IntegrationTest;

mod common;

const HAPPY_CONFIG: &str = include_str!("fixtures/jaeger.router.yaml");
const BROKEN_PLUGIN_CONFIG: &str = include_str!("fixtures/broken_plugin.yaml");
const INVALID_CONFIG: &str = "garbage: garbage";

#[tokio::test(flavor = "multi_thread")]
async fn test_happy() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
    router.start().await;
    router.assert_started().await;
    router.run_query().await;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_config() -> Result<(), BoxError> {
    let mut router = create_router(INVALID_CONFIG).await?;
    router.start().await;
    router.assert_not_started().await;
    router.assert_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reload_config_valid() -> Result<(), BoxError> {
    let mut router = create_router(HAPPY_CONFIG).await?;
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
    let mut router = create_router(HAPPY_CONFIG).await?;
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
    let mut router = create_router(HAPPY_CONFIG).await?;
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

async fn create_router(config: &str) -> Result<IntegrationTest, BoxError> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    Ok(IntegrationTest::new(tracer, opentelemetry_jaeger::Propagator::new(), config).await)
}
