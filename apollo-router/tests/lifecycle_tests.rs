use std::time::Duration;

use apollo_router::graphql;
use serde_json::json;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::common::IntegrationTest;
use crate::common::TracedResponder;

mod common;

const HAPPY_CONFIG: &str = include_str!("fixtures/jaeger.router.yaml");
const BROKEN_PLUGIN_CONFIG: &str = include_str!("fixtures/broken_plugin.router.yaml");
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

#[tokio::test(flavor = "multi_thread")]
async fn test_graceful_shutdown() -> Result<(), BoxError> {
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name("my_app")
        .install_simple()?;

    let mut router = IntegrationTest::with_mock_responder(
        tracer,
        opentelemetry_jaeger::Propagator::new(),
        "telemetry:
  tracing:
    propagation:
      jaeger: true
    trace_config:
      service_name: router
    jaeger:
      batch_processor:
        scheduled_delay: 100ms
      agent:
        endpoint: default
override_subgraph_url:
  products: http://localhost:4005
include_subgraph_errors:
  all: true
",
       TracedResponder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ).set_delay(Duration::from_secs(5))),
    )
    .await;

    router.start().await;
    router.assert_started().await;
    let pid = router.pid();

    let client_handle = tokio::task::spawn(async move {
        let (_, res) = router.run_query().await;
        (
            router,
            serde_json::from_slice::<graphql::Response>(&res.bytes().await.unwrap()).unwrap(),
        )
    });

    tokio::time::sleep(Duration::from_millis(1000)).await;
    #[cfg(target_family = "unix")]
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    #[cfg(not(target_family = "unix"))]
    let _ = self
        .router
        .as_mut()
        .expect("router not started")
        .kill()
        .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let (mut router, data) = client_handle.await.unwrap();
    insta::assert_json_snapshot!(data);
    router.assert_shutdown().await;

    Ok(())
}
