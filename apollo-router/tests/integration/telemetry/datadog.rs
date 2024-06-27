#![cfg(all(target_os = "linux", target_arch = "x86_64", test))]
extern crate core;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use opentelemetry_api::trace::TraceId;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;

use crate::integration::common::Telemetry;
use crate::integration::common::ValueExt;
use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        assert_eq!(
            result
                .headers()
                .get("apollo-custom-trace-id")
                .unwrap()
                .to_str()
                .unwrap(),
            id.to_datadog()
        );
        validate_trace(
            id,
            &query,
            Some("ExampleQuery"),
            &["client", "router", "subgraph"],
            false,
            &[
                "query_planning",
                "client_request",
                "subgraph_request",
                "subgraph",
                "fetch",
                "supergraph",
                "execution",
                "router",
                "query ExampleQuery",
                "subgraph server",
                "http_request",
                "parse_query",
            ],
        )
        .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resource_mapping_default() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_resource_mapping_default.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        println!("dd trace id: {}", id.to_datadog());
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
            &[
                "parse_query",
                "ExampleQuery",
                "client_request",
                "execution",
                "query_planning",
                "products",
                "fetch",
                "subgraph server",
                "ExampleQuery__products__0",
            ],
        )
        .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resource_mapping_override() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_resource_mapping_override.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        println!("dd trace id: {}", id.to_datadog());
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
            &[
                "parse_query",
                "ExampleQuery",
                "client_request",
                "execution",
                "query_planning",
                "products",
                "fetch",
                "subgraph server",
                "ExampleQuery__products__0",
            ],
        )
        .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

async fn validate_trace(
    id: TraceId,
    query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
    custom_span_instrumentation: bool,
    expected_span_names: &[&'static str],
) -> Result<(), BoxError> {
    let datadog_id = id.to_datadog();
    let url = format!("http://localhost:8126/test/traces?trace_ids={datadog_id}");
    for _ in 0..10 {
        if find_valid_trace(
            &url,
            query,
            operation_name,
            services,
            custom_span_instrumentation,
            expected_span_names,
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
        expected_span_names,
    )
    .await?;
    Ok(())
}

async fn find_valid_trace(
    url: &str,
    _query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
    _custom_span_instrumentation: bool,
    expected_span_names: &[&'static str],
) -> Result<(), BoxError> {
    // A valid trace has:
    // * All three services
    // * The correct spans
    // * All spans are parented
    // * Required attributes of 'router' span has been set

    // For now just validate service name.
    let trace: Value = reqwest::get(url)
        .await
        .map_err(|e| anyhow!("failed to contact datadog; {}", e))?
        .json()
        .await?;
    tracing::debug!("{}", serde_json::to_string_pretty(&trace)?);
    verify_trace_participants(&trace, services)?;
    verify_spans_present(&trace, operation_name, services, expected_span_names)?;
    Ok(())
}

fn verify_trace_participants(trace: &Value, services: &[&'static str]) -> Result<(), BoxError> {
    let actual_services: HashSet<String> = trace
        .select_path("$..service")?
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

fn verify_spans_present(
    trace: &Value,
    _operation_name: Option<&str>,
    services: &[&'static str],
    expected_span_names: &[&'static str],
) -> Result<(), BoxError> {
    let operation_names: HashSet<String> = trace
        .select_path("$..resource")?
        .into_iter()
        .filter_map(|span_name| span_name.as_string())
        .collect();
    let mut expected_span_names: HashSet<String> =
        expected_span_names.iter().map(|s| s.to_string()).collect();
    if services.contains(&"client") {
        expected_span_names.insert("client_request".into());
    }
    tracing::debug!("found spans {:?}", operation_names);
    let missing_operation_names: Vec<_> = expected_span_names
        .iter()
        .filter(|o| !operation_names.contains(*o))
        .collect();
    if !missing_operation_names.is_empty() {
        return Err(BoxError::from(format!(
            "spans did not match, got {operation_names:?}, missing {missing_operation_names:?}"
        )));
    }
    Ok(())
}

pub(crate) trait DatadogId {
    fn to_datadog(&self) -> String;
}
impl DatadogId for TraceId {
    fn to_datadog(&self) -> String {
        let bytes = &self.to_bytes()[std::mem::size_of::<u64>()..std::mem::size_of::<u128>()];
        u64::from_be_bytes(bytes.try_into().unwrap()).to_string()
    }
}
