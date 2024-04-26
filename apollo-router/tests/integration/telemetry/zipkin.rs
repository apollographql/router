#![cfg(all(target_os = "linux", target_arch = "x86_64", test))]
extern crate core;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::integration::common::Telemetry;
use crate::integration::common::ValueExt;
use crate::integration::IntegrationTest;

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
            &["client", "router", "subgraph"],
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
        tokio::time::sleep(Duration::from_millis(100)).await;
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
    services: &[&'static str],
    _custom_span_instrumentation: bool,
) -> Result<(), BoxError> {
    // A valid trace has:
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
    verify_trace_participants(&trace, services)?;

    Ok(())
}

fn verify_trace_participants(trace: &Value, services: &[&'static str]) -> Result<(), BoxError> {
    let actual_services: HashSet<String> = trace
        .select_path("$..serviceName")?
        .into_iter()
        .filter_map(|service| service.as_string())
        .collect();
    tracing::debug!("found services {:?}", actual_services);

    let expected_services = services
        .iter()
        .map(|s| s.to_string())
        .collect::<HashSet<_>>();
    if actual_services != expected_services {
        return Err(BoxError::from(format!(
            "incomplete traces, got {actual_services:?} expected {expected_services:?}"
        )));
    }
    Ok(())
}
