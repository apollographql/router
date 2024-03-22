#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

extern crate core;

mod common;

use std::time::Duration;

use anyhow::anyhow;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::common::IntegrationTest;
use crate::common::Telemetry;
use crate::common::ValueExt;

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Zipkin)
        .config(include_str!("fixtures/zipkin.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        assert!(!result
            .headers()
            .get("apollo-custom-trace-id")
            .unwrap()
            .is_empty());
        validate_trace(
            id,
            &query,
            Some("ExampleQuery"),
            &["my_app", "router", "products"],
            false,
        )
        .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

async fn validate_trace(
    id: String,
    query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
    custom_span_instrumentation: bool,
) -> Result<(), BoxError> {
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("service", services.first().expect("expected root service"))
        .finish();

    let url = format!("http://localhost:9411/api/v2/trace/{id}?{params}");
    for _ in 0..10 {
        if find_valid_trace(
            &url,
            query,
            operation_name,
            services,
            custom_span_instrumentation,
        )
        .await
        .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    find_valid_trace(
        &url,
        query,
        operation_name,
        services,
        custom_span_instrumentation,
    )
    .await?;
    Ok(())
}

async fn find_valid_trace(
    url: &str,
    _query: &Value,
    _operation_name: Option<&str>,
    _services: &[&'static str],
    _custom_span_instrumentation: bool,
) -> Result<(), BoxError> {
    // A valid trace has:
    // * A valid service name
    // * All three services
    // * The correct spans
    // * All spans are parented
    // * Required attributes of 'router' span has been set

    // For now just validate service name.
    let trace: Value = reqwest::get(url)
        .await
        .map_err(|e| anyhow!("failed to contact zipkin; {}", e))?
        .json()
        .await?;
    tracing::debug!("{}", serde_json::to_string_pretty(&trace)?);
    let service_name = trace.select_path("$..localEndpoint.serviceName")?;

    assert_eq!(
        service_name.first(),
        Some(&&Value::String("router".to_string()))
    );

    Ok(())
}
