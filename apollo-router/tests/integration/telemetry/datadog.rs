extern crate core;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::anyhow;
use opentelemetry_api::trace::SpanContext;
use opentelemetry_api::trace::TraceContextExt;
use opentelemetry_api::trace::TraceId;
use opentelemetry_api::Context;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
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
    priority_sampled: Option<&'static str>,
    // Not the metrics but the otel attribute
    no_priority_sampled_attribute: Option<bool>,
}

#[tokio::test(flavor = "multi_thread")]
async fn test_no_sample() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_clone = context.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog_no_sample.router.yaml"))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let context = Context::current();
            let span = context.span();
            let span_context = span.span_context();
            *context_clone.lock().expect("poisoned") = Some(span_context.clone());
        }))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (_id, result) = router.execute_untraced_query(&query, None).await;
    router.graceful_shutdown().await;
    assert!(result.status().is_success());
    let context = context
        .lock()
        .expect("poisoned")
        .as_ref()
        .expect("state")
        .clone();
    assert!(context.is_sampled());
    assert_eq!(context.trace_state().get("psr"), Some("0"));

    Ok(())
}

// We want to check we're able to override the behavior of preview_datadog_agent_sampling configuration even if we set a datadog exporter
#[tokio::test(flavor = "multi_thread")]
async fn test_sampling_datadog_agent_disabled() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_clone = context.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_agent_sampling_disabled.router.yaml"
        ))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let context = Context::current();
            let span = context.span();
            let span_context = span.span_context();
            *context_clone.lock().expect("poisoned") = Some(span_context.clone());
        }))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, result) = router.execute_untraced_query(&query, None).await;
    router.graceful_shutdown().await;
    assert!(result.status().is_success());
    let _context = context
        .lock()
        .expect("poisoned")
        .as_ref()
        .expect("state")
        .clone();

    tokio::time::sleep(Duration::from_secs(2)).await;
    TraceSpec::builder()
        .services([].into())
        .build()
        .validate_trace(id)
        .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_clone = context.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let context = Context::current();
            let span = context.span();
            let span_context = span.span_context();
            *context_clone.lock().expect("poisoned") = Some(span_context.clone());
        }))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Parent based sampling. psr MUST be populated with the value that we pass in.
    test_psr(
        &context,
        &mut router,
        Some("-1"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("-1")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("0"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("0")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("1"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("2"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("2")
            .build(),
    )
    .await?;

    // No psr was passed in the router is free to set it. This will be 1 as we are going to sample here.
    test_psr(
        &context,
        &mut router,
        None,
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated_otel_request() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_clone = context.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .config(include_str!("fixtures/datadog.router.yaml"))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let context = Context::current();
            let span = context.span();
            let span_context = span.span_context();
            *context_clone.lock().expect("poisoned") = Some(span_context.clone());
        }))
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
    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("1")
        .build()
        .validate_trace(id)
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_no_parent_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let context = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_clone = context.clone();
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_no_parent_sampler.router.yaml"
        ))
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .subgraph_callback(Box::new(move || {
            let context = Context::current();
            let span = context.span();
            let span_context = span.span_context();
            *context_clone.lock().expect("poisoned") = Some(span_context.clone());
        }))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // The router will ignore the upstream PSR as parent based sampling is disabled.
    test_psr(
        &context,
        &mut router,
        Some("-1"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("0"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("1"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;
    test_psr(
        &context,
        &mut router,
        Some("2"),
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;

    test_psr(
        &context,
        &mut router,
        None,
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .priority_sampled("1")
            .build(),
    )
    .await?;

    router.graceful_shutdown().await;

    Ok(())
}

async fn test_psr(
    context: &Arc<Mutex<Option<SpanContext>>>,
    router: &mut IntegrationTest,
    psr: Option<&str>,
    trace_spec: TraceSpec,
) -> Result<(), BoxError> {
    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let headers = if let Some(psr) = psr {
        vec![("x-datadog-sampling-priority".to_string(), psr.to_string())]
    } else {
        vec![]
    };
    let (id, result) = router
        .execute_query_with_headers(&query, headers.into_iter().collect())
        .await;

    assert!(result.status().is_success());
    let context = context
        .lock()
        .expect("poisoned")
        .as_ref()
        .expect("state")
        .clone();

    assert_eq!(
        context.trace_state().get("psr"),
        trace_spec.priority_sampled
    );
    trace_spec.validate_trace(id).await?;
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
        for _ in 0..20 {
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
        self.verify_measured_spans(&trace)?;
        self.verify_operation_name(&trace)?;
        self.verify_priority_sampled(&trace)?;
        self.verify_priority_sampled_attribute(&trace)?;
        self.verify_version(&trace)?;
        self.verify_span_kinds(&trace)?;
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

    fn verify_measured_spans(&self, trace: &Value) -> Result<(), BoxError> {
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

    fn verify_span_kinds(&self, trace: &Value) -> Result<(), BoxError> {
        // Validate that the span.kind has been propagated. We can just do this for a selection of spans.
        if self.services.contains("router") {
            self.validate_span_kind(trace, "router", "server")?;
            self.validate_span_kind(trace, "supergraph", "internal")?;
            self.validate_span_kind(trace, "http_request", "client")?;
        }
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

        if binding.is_none() {
            return Err(BoxError::from(format!(
                "span.kind missing or incorrect {}, {}",
                name, trace
            )));
        }

        let binding = binding
            .expect("expected binding")
            .as_str()
            .expect("expected string");
        if binding != kind {
            return Err(BoxError::from(format!(
                "span.kind mismatch, expected {} got {}",
                kind, binding
            )));
        }

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
        if let Some(psr) = self.priority_sampled {
            let binding =
                trace.select_path("$..[?(@.service=='router')].metrics._sampling_priority_v1")?;
            if binding.is_empty() {
                return Err(BoxError::from("missing sampling priority"));
            }
            for sampling_priority in binding {
                assert_eq!(
                    sampling_priority
                        .as_f64()
                        .expect("psr not string")
                        .to_string(),
                    psr
                );
            }
        }
        Ok(())
    }

    fn verify_priority_sampled_attribute(&self, trace: &Value) -> Result<(), BoxError> {
        if self.no_priority_sampled_attribute.unwrap_or_default() {
            let binding =
                trace.select_path("$..[?(@.service=='router')].meta['sampling.priority']")?;
            if binding.is_empty() {
                return Ok(());
            } else {
                return Err(BoxError::from("sampling priority attribute exists"));
            }
        }
        Ok(())
    }
}
