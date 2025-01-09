extern crate core;

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Deref;
use std::time::Duration;

use anyhow::anyhow;
use opentelemetry::trace::TraceId;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceResponse;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use serde_json::Value;
use tower::BoxError;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::Times;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::integration::IntegrationTest;
use crate::integration::ValueExt;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;
use crate::integration::telemetry::DatadogId;
use crate::integration::telemetry::TraceSpec;
use crate::integration::telemetry::verifier::Verifier;

#[tokio::test(flavor = "multi_thread")]
async fn test_trace_error() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server_delayed().await;
    let config = include_str!("fixtures/otlp_invalid_endpoint.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_log_contained("OpenTelemetry trace error occurred: cannot send message to batch processor 'otlp-tracing' as the channel is full");
    router.assert_metrics_contains(r#"apollo_router_telemetry_batch_processor_errors_total{error="channel full",name="otlp-tracing",otel_scope_name="apollo/router"}"#, None).await;
    router.graceful_shutdown().await;

    drop(mock_server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    for _ in 0..2 {
        TraceSpec::builder()
            .operation_name("ExampleQuery")
            .services(["client", "router", "subgraph"].into())
            .span_names(
                [
                    "query_planning",
                    "client_request",
                    "ExampleQuery__products__0",
                    "fetch",
                    "execution",
                    "query ExampleQuery",
                    "subgraph server",
                    "parse_query",
                    "http_request",
                ]
                .into(),
            )
            .subgraph_sampled(true)
            .build()
            .validate_otlp_trace(&mut router, &mock_server, Query::default())
            .await?;
        TraceSpec::builder()
            .service("router")
            .build()
            .validate_otlp_metrics(&mock_server)
            .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
        router.assert_log_not_contained("OpenTelemetry metric error occurred: Metrics error: metrics provider already shut down");
    }
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resources() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .resource("env", "local1")
        .resource("service.version", "router_version_override")
        .resource("service.name", "router")
        .build()
        .validate_otlp_trace(&mut router, &mock_server, Query::default())
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_datadog_propagator() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_propagation.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(&mut router, &mock_server, Query::default())
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_datadog_propagator_no_agent() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_propagation_no_agent.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_zipkin_trace_context_propagator_with_datadog()
-> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_request_with_zipkin_propagator.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).build(),
        )
        .await?;
    // ---------------------- zipkin propagator with unsampled trace
    // Testing for an unsampled trace, so it should be sent to the otlp exporter with sampling priority set 0
    // But it shouldn't send the trace to subgraph as the trace is originally not sampled, the main goal is to measure it at the DD agent level
    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder()
                .traced(false)
                .header("X-B3-TraceId", "80f198ee56343ba864fe8b2a57d3eff7")
                .header("X-B3-ParentSpanId", "05e3ac9a4f6e3b90")
                .header("X-B3-SpanId", "e457b5a2e4d86bd1")
                .header("X-B3-Sampled", "0")
                .build(),
        )
        .await?;
    // ---------------------- trace context propagation
    // Testing for a trace containing the right tracestate with m and psr for DD and a sampled trace, so it should be sent to the otlp exporter with sampling priority set to 1
    // And it should also send the trace to subgraph as the trace is sampled
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder()
                .traced(true)
                .header(
                    "traceparent",
                    "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
                )
                .header("tracestate", "m=1,psr=1")
                .build(),
        )
        .await?;
    // ----------------------
    // Testing for a trace containing the right tracestate with m and psr for DD and an unsampled trace, so it should be sent to the otlp exporter with sampling priority set to 0
    // But it shouldn't send the trace to subgraph as the trace is originally not sampled, the main goal is to measure it at the DD agent level
    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder()
                .traced(false)
                .header(
                    "traceparent",
                    "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-02",
                )
                .header("tracestate", "m=1,psr=0")
                .build(),
        )
        .await?;
    // ----------------------
    // Testing for a trace containing a tracestate m and psr with psr set to 1 for DD and an unsampled trace, so it should be sent to the otlp exporter with sampling priority set to 1
    // It should not send the trace to the subgraph as we didn't use the datadog propagator and therefore the trace will remain unsampled.
    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder()
                .traced(false)
                .header(
                    "traceparent",
                    "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-03",
                )
                .header("tracestate", "m=1,psr=1")
                .build(),
        )
        .await?;

    // Be careful if you add the same kind of test crafting your own trace id, make sure to increment the previous trace id by 1 if not you'll receive all the previous spans tested with the same trace id before
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_no_sample_datadog_agent() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_agent_no_sample.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .config(&config)
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(false).build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_sample_datadog_agent() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_agent_sample.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .config(&config)
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(false).build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_sample_datadog_agent_unsampled() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_agent_sample_no_sample.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(false).build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_propagation.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        // We're using datadog propagation as this is what we are trying to test.
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Parent based sampling. psr MUST be populated with the value that we pass in.
    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("-1")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("-1").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("0").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("1").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("2")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("2").build(),
        )
        .await?;

    // No psr was passed in the router is free to set it. This will be 1 as we are going to sample here.
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).build(),
        )
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_parent_sampler_very_small_no_parent_no_agent_sampling()
-> Result<(), BoxError> {
    // Note that there is a very small chance this test will fail. We are trying to test a non-zero sampler.
    let mock_server = mock_otlp_server(0..).await;

    if !graph_os_enabled() {
        return Ok(());
    }
    let config = include_str!(
        "fixtures/otlp_parent_sampler_very_small_no_parent_no_agent_sampling.router.yaml"
    )
    .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // The router should not respect upstream but also almost never sample if left to its own devices.

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).build(),
        )
        .await?;

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().psr("-1").traced(true).build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().psr("0").traced(true).build(),
        )
        .await?;

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().psr("1").traced(true).build(),
        )
        .await?;

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().psr("2").traced(true).build(),
        )
        .await?;

    TraceSpec::builder()
        .services([].into())
        .subgraph_sampled(false)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(false).build(),
        )
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_no_parent_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_datadog_propagation_no_parent_sampler.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // The router will ignore the upstream PSR as parent based sampling is disabled.

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("-1").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("0").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("1").build(),
        )
        .await?;
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).psr("2").build(),
        )
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder().traced(true).build(),
        )
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_attributes() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .attribute("client.name", "foobar")
        .build()
        .validate_otlp_trace(
            &mut router,
            &mock_server,
            Query::builder()
                .traced(true)
                .header("apollographql-client-name", "foobar")
                .build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_plugin_overridden_client_name_is_included_in_telemetry() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/otlp_override_client_name.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // rhai script overrides client.name - no matter what client name we pass via headers, it should
    // end up equalling the value set in the script (`foo`)
    for header_value in [None, Some(""), Some("foo"), Some("bar")] {
        let mut headers = HashMap::default();
        if let Some(value) = header_value {
            headers.insert("apollographql-client-name".to_string(), value.to_string());
        }

        let query = Query::builder().traced(true).headers(headers).build();
        TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .attribute("client.name", "foo")
            .build()
            .validate_otlp_trace(&mut router, &mock_server, query)
            .await
            .unwrap_or_else(|_| panic!("Failed with header value {header_value:?}"));
    }

    router.graceful_shutdown().await;
    Ok(())
}

struct OtlpTraceSpec<'a> {
    trace_spec: TraceSpec,
    mock_server: &'a MockServer,
}
impl Deref for OtlpTraceSpec<'_> {
    type Target = TraceSpec;

    fn deref(&self) -> &Self::Target {
        &self.trace_spec
    }
}

impl Verifier for OtlpTraceSpec<'_> {
    fn spec(&self) -> &TraceSpec {
        &self.trace_spec
    }

    fn measured_span(&self, trace: &Value, name: &str) -> Result<bool, BoxError> {
        let binding1 = trace.select_path(&format!(
            "$..[?(@.meta.['otel.original_name'] == '{name}')].metrics.['_dd.measured']"
        ))?;
        let binding2 = trace.select_path(&format!(
            "$..[?(@.name == '{name}')].metrics.['_dd.measured']"
        ))?;
        Ok(binding1
            .first()
            .or(binding2.first())
            .and_then(|v| v.as_f64())
            .map(|v| v == 1.0)
            .unwrap_or_default())
    }

    async fn find_valid_metrics(&self) -> Result<(), BoxError> {
        let requests = self
            .mock_server
            .received_requests()
            .await
            .expect("Could not get otlp requests");
        if let Some(metrics) = requests.iter().find(|r| r.url.path().ends_with("/metrics")) {
            let metrics = opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest::decode(bytes::Bytes::copy_from_slice(&metrics.body))?;
            let json_metrics = serde_json::to_value(metrics)?;
            // For now just validate service name.
            self.verify_services(&json_metrics)?;

            Ok(())
        } else {
            Err(anyhow!("No metrics received").into())
        }
    }

    async fn get_trace(&self, trace_id: TraceId) -> Result<Value, axum::BoxError> {
        let requests = self.mock_server.received_requests().await;
        let trace = Value::Array(requests.unwrap_or_default().iter().filter(|r| r.url.path().ends_with("/traces"))
            .filter_map(|r| {
                match opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest::decode(
                    bytes::Bytes::copy_from_slice(&r.body),
                ) {
                    Ok(trace) => {
                        serde_json::to_value(trace).ok()
                    }
                    Err(_) => {
                        None
                    }
                }
            }).filter(|t| {
            let datadog_trace_id = TraceId::from_u128(trace_id.to_datadog() as u128);
            let trace_found1 = !t.select_path(&format!("$..[?(@.traceId == '{trace_id}')]")).unwrap_or_default().is_empty();
            let trace_found2 = !t.select_path(&format!("$..[?(@.traceId == '{datadog_trace_id}')]")).unwrap_or_default().is_empty();
            trace_found1 | trace_found2
        }).collect());
        Ok(trace)
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

    fn verify_services(&self, trace: &Value) -> Result<(), axum::BoxError> {
        let actual_services: HashSet<String> = trace
            .select_path("$..resource.attributes..[?(@.key == 'service.name')].value.stringValue")?
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
                "unexpected traces, got {actual_services:?} expected {expected_services:?}"
            )));
        }
        Ok(())
    }

    fn verify_spans_present(&self, trace: &Value) -> Result<(), BoxError> {
        let operation_names: HashSet<String> = trace
            .select_path("$..spans..name")?
            .into_iter()
            .filter_map(|span_name| span_name.as_string())
            .collect();
        let mut span_names: HashSet<&str> = self.span_names.clone();
        if self.services.contains(&"client") {
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
        let kind = match kind {
            "internal" => 1,
            "client" => 3,
            "server" => 2,
            _ => panic!("unknown kind"),
        };
        let binding1 = trace.select_path(&format!(
            "$..spans..[?(@.kind == {kind})]..[?(@.key == 'otel.original_name')].value..[?(@ == '{name}')]"
        ))?;
        let binding2 = trace.select_path(&format!(
            "$..spans..[?(@.kind == {kind} && @.name == '{name}')]"
        ))?;
        let binding = binding1.first().or(binding2.first());

        if binding.is_none() {
            return Err(BoxError::from(format!(
                "span.kind missing or incorrect {name}, {kind}"
            )));
        }
        Ok(())
    }

    fn verify_operation_name(&self, trace: &Value) -> Result<(), BoxError> {
        if let Some(expected_operation_name) = &self.operation_name {
            let binding =
                trace.select_path("$..[?(@.name == 'supergraph')]..[?(@.key == 'graphql.operation.name')].value.stringValue")?;
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
            let binding = trace.select_path(
                "$..[?(@.name == 'execution')]..[?(@.key == 'sampling.priority')].value.intValue",
            )?;
            if binding.is_empty() {
                return Err(BoxError::from("missing sampling priority"));
            }
            for sampling_priority in binding {
                assert_eq!(sampling_priority.as_str().expect("psr not a string"), psr);
            }
        } else {
            assert!(trace.select_path("$..[?(@.name == 'execution')]..[?(@.key == 'sampling.priority')].value.intValue")?.is_empty())
        }
        Ok(())
    }

    fn verify_resources(&self, trace: &Value) -> Result<(), BoxError> {
        if !self.resources.is_empty() {
            let resources = trace.select_path("$..resource.attributes")?;
            // Find the attributes for the router service
            let router_resources = resources
                .iter()
                .filter(|r| {
                    !r.select_path("$..[?(@.stringValue == 'router')]")
                        .unwrap()
                        .is_empty()
                })
                .collect::<Vec<_>>();
            // Let's map this to a map of key value pairs
            let router_resources = router_resources
                .iter()
                .flat_map(|v| v.as_array().expect("array required"))
                .map(|v| {
                    let entry = v.as_object().expect("must be an object");
                    (
                        entry
                            .get("key")
                            .expect("must have key")
                            .as_string()
                            .expect("key must be a string"),
                        entry
                            .get("value")
                            .expect("must have value")
                            .as_object()
                            .expect("value must be an object")
                            .get("stringValue")
                            .expect("value must be a string")
                            .as_string()
                            .expect("value must be a string"),
                    )
                })
                .collect::<HashMap<_, _>>();

            for (key, value) in &self.resources {
                if let Some(actual_value) = router_resources.get(*key) {
                    assert_eq!(actual_value, value);
                } else {
                    return Err(BoxError::from(format!("missing resource key: {}", *key)));
                }
            }
        }
        Ok(())
    }

    fn verify_span_attributes(&self, trace: &Value) -> Result<(), BoxError> {
        for (key, value) in self.attributes.iter() {
            // extracts a list of span attribute values with the provided key
            let binding = trace.select_path(&format!(
                "$..spans..attributes..[?(@.key == '{key}')].value.*"
            ))?;
            let matches_value = binding.iter().any(|v| match v {
                Value::Bool(v) => (*v).to_string() == *value,
                Value::Number(n) => (*n).to_string() == *value,
                Value::String(s) => s == value,
                _ => false,
            });
            if !matches_value {
                return Err(BoxError::from(format!(
                    "unexpected attribute values for key `{key}`, expected value `{value}` but got {binding:?}"
                )));
            }
        }
        Ok(())
    }
}

async fn mock_otlp_server_delayed() -> MockServer {
    let mock_server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_raw(
                    ExportTraceServiceResponse::default().encode_to_vec(),
                    "application/x-protobuf",
                ),
        )
        .mount(&mock_server)
        .await;

    mock_server
}

async fn mock_otlp_server<T: Into<Times> + Clone>(expected_requests: T) -> MockServer {
    let mock_server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            ExportTraceServiceResponse::default().encode_to_vec(),
            "application/x-protobuf",
        ))
        .expect(expected_requests.clone())
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            ExportMetricsServiceResponse::default().encode_to_vec(),
            "application/x-protobuf",
        ))
        .expect(expected_requests.clone())
        .mount(&mock_server)
        .await;
    mock_server
}

impl TraceSpec {
    async fn validate_otlp_trace(
        self,
        router: &mut IntegrationTest,
        mock_server: &MockServer,
        query: Query,
    ) -> Result<(), BoxError> {
        OtlpTraceSpec {
            trace_spec: self,
            mock_server,
        }
        .validate_trace(router, query)
        .await
    }
    async fn validate_otlp_metrics(self, mock_server: &MockServer) -> Result<(), BoxError> {
        OtlpTraceSpec {
            trace_spec: self,
            mock_server,
        }
        .validate_metrics()
        .await
    }
}
