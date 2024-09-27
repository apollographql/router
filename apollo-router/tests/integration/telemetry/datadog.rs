extern crate core;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::anyhow;
use opentelemetry_api::trace::TraceContextExt;
use opentelemetry_api::trace::TraceId;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use wiremock::ResponseTemplate;

use crate::integration::common::graph_os_enabled;
use crate::integration::common::Telemetry;
use crate::integration::IntegrationTest;
use crate::integration::ValueExt;

#[derive(buildstructor::Builder)]
struct TraceSpec {
    operation_name: Option<String>,
    version: Option<String>,
    services: HashSet<&'static str>,
    span_names: HashSet<&'static str>,
    measured_spans: HashSet<&'static str>,
    unmeasured_spans: HashSet<&'static str>,
}

#[tokio::test(flavor = "multi_thread")]
async fn test_no_sample() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let subgraph_was_sampled = std::sync::Arc::new(AtomicBool::new(false));
    let subgraph_was_sampled_callback = subgraph_was_sampled.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog_no_sample.router.yaml"))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let sampled = Span::current().context().span().span_context().is_sampled();
            subgraph_was_sampled_callback.store(sampled, std::sync::atomic::Ordering::SeqCst);
        }))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (_id, result) = router.execute_untraced_query(&query).await;
    router.graceful_shutdown().await;
    assert!(result.status().is_success());
    assert!(!subgraph_was_sampled.load(std::sync::atomic::Ordering::SeqCst));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_default_span_names() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_default_span_names.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
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
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "query_planning",
                "client_request",
                "subgraph_request",
                "subgraph",
                "fetch",
                "supergraph",
                "execution",
                "query ExampleQuery",
                "subgraph server",
                "http_request",
                "parse_query",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_override_span_names() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_override_span_names.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
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
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "query_planning",
                "client_request",
                "subgraph_request",
                "subgraph",
                "fetch",
                "supergraph",
                "execution",
                "overridden",
                "subgraph server",
                "http_request",
                "parse_query",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_override_span_names_late() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_override_span_names_late.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
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
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "query_planning",
                "client_request",
                "subgraph_request",
                "subgraph",
                "fetch",
                "supergraph",
                "execution",
                "ExampleQuery",
                "subgraph server",
                "http_request",
                "parse_query",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
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
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "query_planning",
                "client_request",
                "ExampleQuery__products__0",
                "products",
                "fetch",
                "/",
                "execution",
                "ExampleQuery",
                "subgraph server",
                "parse_query",
            ]
            .into(),
        )
        .measured_spans(
            [
                "query_planning",
                "subgraph",
                "http_request",
                "subgraph_request",
                "router",
                "execution",
                "supergraph",
                "parse_query",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_with_parent_span() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let mut headers = HashMap::new();
    headers.insert(
        "traceparent".to_string(),
        String::from("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
    );
    let (id, result) = router.execute_query_with_headers(&query, headers).await;
    assert_eq!(
        result
            .headers()
            .get("apollo-custom-trace-id")
            .unwrap()
            .to_str()
            .unwrap(),
        id.to_datadog()
    );
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "query_planning",
                "client_request",
                "ExampleQuery__products__0",
                "products",
                "fetch",
                "/",
                "execution",
                "ExampleQuery",
                "subgraph server",
                "parse_query",
            ]
            .into(),
        )
        .measured_spans(
            [
                "query_planning",
                "subgraph",
                "http_request",
                "subgraph_request",
                "router",
                "execution",
                "supergraph",
                "parse_query",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resource_mapping_default() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
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
    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "parse_query",
                "/",
                "ExampleQuery",
                "client_request",
                "execution",
                "query_planning",
                "products",
                "fetch",
                "subgraph server",
                "ExampleQuery__products__0",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resource_mapping_override() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
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
    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "parse_query",
                "ExampleQuery",
                "client_request",
                "execution",
                "query_planning",
                "products",
                "fetch",
                "subgraph server",
                "overridden",
                "ExampleQuery__products__0",
            ]
            .into(),
        )
        .build()
        .validate_trace(id)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_span_metrics() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/disable_span_metrics.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, result) = router.execute_query(&query).await;
    assert!(!result
        .headers()
        .get("apollo-custom-trace-id")
        .unwrap()
        .is_empty());
    router.graceful_shutdown().await;
    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .services(["client", "router", "subgraph"].into())
        .span_names(
            [
                "parse_query",
                "ExampleQuery",
                "client_request",
                "execution",
                "query_planning",
                "products",
                "fetch",
                "subgraph server",
                "ExampleQuery__products__0",
            ]
            .into(),
        )
        .measured_span("subgraph")
        .unmeasured_span("supergraph")
        .build()
        .validate_trace(id)
        .await?;
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

impl TraceSpec {
    #[allow(clippy::too_many_arguments)]
    async fn validate_trace(&self, id: TraceId) -> Result<(), BoxError> {
        let datadog_id = id.to_datadog();
        let url = format!("http://localhost:8126/test/traces?trace_ids={datadog_id}");
        for _ in 0..10 {
            if self.find_valid_trace(&url).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.find_valid_trace(&url).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn find_valid_trace(&self, url: &str) -> Result<(), BoxError> {
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
        self.verify_trace_participants(&trace)?;
        self.verify_spans_present(&trace)?;
        self.validate_measured_spans(&trace)?;
        self.verify_operation_name(&trace)?;
        self.verify_priority_sampled(&trace)?;
        self.verify_version(&trace)?;
        self.validate_span_kinds(&trace)?;
        Ok(())
    }

    fn verify_version(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_version) = &self.version {
            let binding = trace.select_path("$..version")?;
            let version = binding.first();
            assert_eq!(
                version
                    .expect("version expected")
                    .as_str()
                    .expect("version must be a string"),
                expected_version
            );
        }
        Ok(())
    }

    fn validate_measured_spans(&self, trace: &Value) -> Result<(), BoxError> {
        for expected in &self.measured_spans {
            assert!(
                self.measured_span(trace, expected)?,
                "missing measured span {}",
                expected
            );
        }
        for unexpected in &self.unmeasured_spans {
            assert!(
                !self.measured_span(trace, unexpected)?,
                "unexpected measured span {}",
                unexpected
            );
        }
        Ok(())
    }

    fn measured_span(&self, trace: &Value, name: &str) -> Result<bool, BoxError> {
        let binding1 = trace.select_path(&format!(
            "$..[?(@.meta.['otel.original_name'] == '{}')].metrics.['_dd.measured']",
            name
        ))?;
        let binding2 = trace.select_path(&format!(
            "$..[?(@.name == '{}')].metrics.['_dd.measured']",
            name
        ))?;
        Ok(binding1
            .first()
            .or(binding2.first())
            .and_then(|v| v.as_f64())
            .map(|v| v == 1.0)
            .unwrap_or_default())
    }

    fn validate_span_kinds(&self, trace: &Value) -> Result<(), BoxError> {
        // Validate that the span.kind has been propagated. We can just do this for a selection of spans.
        self.validate_span_kind(trace, "router", "server")?;
        self.validate_span_kind(trace, "supergraph", "internal")?;
        self.validate_span_kind(trace, "http_request", "client")?;
        Ok(())
    }

    fn verify_trace_participants(&self, trace: &Value) -> Result<(), BoxError> {
        let actual_services: HashSet<String> = trace
            .select_path("$..service")?
            .into_iter()
            .filter_map(|service| service.as_string())
            .collect();
        tracing::debug!("found services {:?}", actual_services);

        let expected_services = self
            .services
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

    fn verify_spans_present(&self, trace: &Value) -> Result<(), BoxError> {
        let operation_names: HashSet<String> = trace
            .select_path("$..resource")?
            .into_iter()
            .filter_map(|span_name| span_name.as_string())
            .collect();
        let mut span_names: HashSet<&str> = self.span_names.clone();
        if self.services.contains("client") {
            span_names.insert("client_request");
        }
        tracing::debug!("found spans {:?}", operation_names);
        let missing_operation_names: Vec<_> = span_names
            .iter()
            .filter(|o| !operation_names.contains(**o))
            .collect();
        if !missing_operation_names.is_empty() {
            return Err(BoxError::from(format!(
                "spans did not match, got {operation_names:?}, missing {missing_operation_names:?}"
            )));
        }
        Ok(())
    }

    fn validate_span_kind(&self, trace: &Value, name: &str, kind: &str) -> Result<(), BoxError> {
        let binding1 = trace.select_path(&format!(
            "$..[?(@.meta.['otel.original_name'] == '{}')].meta.['span.kind']",
            name
        ))?;
        let binding2 =
            trace.select_path(&format!("$..[?(@.name == '{}')].meta.['span.kind']", name))?;
        let binding = binding1.first().or(binding2.first());

        assert!(
            binding.is_some(),
            "span.kind missing or incorrect {}, {}",
            name,
            trace
        );
        assert_eq!(
            binding
                .expect("expected binding")
                .as_str()
                .expect("expected string"),
            kind
        );
        Ok(())
    }

    fn verify_operation_name(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_operation_name) = &self.operation_name {
            let binding =
                trace.select_path("$..[?(@.name == 'supergraph')]..['graphql.operation.name']")?;
            let operation_name = binding.first();
            assert_eq!(
                operation_name
                    .expect("graphql.operation.name expected")
                    .as_str()
                    .expect("graphql.operation.name must be a string"),
                expected_operation_name
            );
        }
        Ok(())
    }

    fn verify_priority_sampled(&self, trace: &Value) -> Result<(), BoxError> {
        let binding = trace.select_path("$.._sampling_priority_v1")?;
        let sampling_priority = binding.first();
        // having this priority set to 1.0 everytime is not a problem as we're doing pre sampling in the full telemetry stack
        // So basically if the trace was not sampled it wouldn't get to this stage and so nothing would be sent
        assert_eq!(
            sampling_priority
                .expect("sampling priority expected")
                .as_f64()
                .expect("sampling priority must be a number"),
            1.0
        );
        Ok(())
    }
}
