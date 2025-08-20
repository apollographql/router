extern crate core;

use std::collections::HashSet;
use std::ops::Deref;

use anyhow::anyhow;
use opentelemetry::trace::TraceId;
use serde_json::Value;
use tower::BoxError;

use crate::integration::IntegrationTest;
use crate::integration::ValueExt;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;
use crate::integration::telemetry::DatadogId;
use crate::integration::telemetry::TraceSpec;
use crate::integration::telemetry::verifier::Verifier;

#[tokio::test(flavor = "multi_thread")]
async fn test_no_sample() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog_no_sample.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    TraceSpec::builder()
        .services(["router"].into())
        .subgraph_sampled(false)
        .priority_sampled("0")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

// We want to check we're able to override the behavior of preview_datadog_agent_sampling configuration even if we set a datadog exporter
#[tokio::test(flavor = "multi_thread")]
async fn test_sampling_datadog_agent_disabled() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_agent_sampling_disabled.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services([].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;
    router.graceful_shutdown().await;

    Ok(())
}

// We want to check we're able to override the behavior of preview_datadog_agent_sampling configuration even if we set a datadog exporter
#[tokio::test(flavor = "multi_thread")]
async fn test_sampling_datadog_agent_disabled_always_sample() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_agent_sampling_disabled_1.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sampling_datadog_agent_disabled_never_sample() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_agent_sampling_disabled_0.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services([].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated() -> Result<(), BoxError> {
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

    // Parent based sampling. psr MUST be populated with the value that we pass in.
    TraceSpec::builder()
        .services(["client", "router"].into())
        .subgraph_sampled(false)
        .priority_sampled("-1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("-1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router"].into())
        .subgraph_sampled(false)
        .priority_sampled("0")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("0").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("2")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("2").build())
        .await?;

    // No psr was passed in the router is free to set it. This will be 1 as we are going to sample here.
    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .subgraph_sampled(true)
        .priority_sampled("1")
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated_otel_request() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .extra_propagator(Telemetry::Datadog)
        .config(include_str!("fixtures/datadog.router.yaml"))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_no_parent_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_no_parent_sampler.router.yaml"
        ))
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("-1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("0").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("2").build())
        .await?;

    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_parent_sampler_very_small() -> Result<(), BoxError> {
    // Note that there is a very small chance this test will fail. We are trying to test a non-zero sampler.

    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_parent_sampler_very_small.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // The router should respect upstream but also almost never sample if left to its own devices.
    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("-1")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("-1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("0").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("1").build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("2")
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).psr("2").build())
        .await?;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_parent_sampler_very_small_no_parent() -> Result<(), BoxError> {
    // Note that there is a very small chance this test will fail. We are trying to test a non-zero sampler.

    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_parent_sampler_very_small_no_parent.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // // The router should respect upstream but also almost never sample if left to its own devices.
    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("-1").traced(true).build())
        .await?;
    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("0").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("1").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("2").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_parent_sampler_very_small_no_parent_no_agent_sampling()
-> Result<(), BoxError> {
    // Note that there is a very small chance this test will fail. We are trying to test a non-zero sampler.

    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_parent_sampler_very_small_no_parent_no_agent_sampling.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // // The router should not respect upstream but also almost never sample if left to its own devices.
    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("-1").traced(true).build())
        .await?;
    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("0").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("1").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("2").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services([].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_parent_sampler_very_small_parent_no_agent_sampling()
-> Result<(), BoxError> {
    // Note that there is a very small chance this test will fail. We are trying to test a non-zero sampler.

    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_parent_sampler_very_small_parent_no_agent_sampling.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // // The router should respect upstream but also almost never sample if left to its own devices.
    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("-1").traced(true).build())
        .await?;
    TraceSpec::builder()
        .services(["client"].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("0").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("1").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .subgraph_sampled(true)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().psr("2").traced(true).build())
        .await?;

    TraceSpec::builder()
        .services([].into())
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_parent_sampler_very_small.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    TraceSpec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .subgraph_sampled(false)
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(false).build())
        .await?;

    router.graceful_shutdown().await;

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

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
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

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
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
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_header_propagator_override() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(include_str!(
            "fixtures/datadog_header_propagator_override.router.yaml"
        ))
        .build()
        .await;

    let trace_id = opentelemetry::trace::TraceId::from_u128(uuid::Uuid::new_v4().as_u128());

    router.start().await;
    router.assert_started().await;
    TraceSpec::builder()
        .services(["router", "subgraph"].into())
        .subgraph_sampled(true)
        .trace_id(format!("{:032x}", trace_id.to_datadog()))
        .build()
        .validate_datadog_trace(
            &mut router,
            Query::builder()
                .header("trace-id", trace_id.to_string())
                .header("x-datadog-trace-id", "2")
                .traced(false)
                .build(),
        )
        .await?;
    router.graceful_shutdown().await;
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

    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .priority_sampled("1")
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
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
        .validate_datadog_trace(
            &mut router,
            Query::builder()
                .traced(true)
                .header(
                    "traceparent",
                    "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
                )
                .build(),
        )
        .await?;
    router.graceful_shutdown().await;
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
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
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_resources() -> Result<(), BoxError> {
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

    TraceSpec::builder()
        .operation_name("ExampleQuery")
        .resource("env", "local1")
        .resource("service.version", "router_version_override")
        .resource("service.name", "router")
        .services(["client", "router", "subgraph"].into())
        .build()
        .validate_datadog_trace(&mut router, Query::builder().traced(true).build())
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_attributes() -> Result<(), BoxError> {
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

    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .attribute("client.name", "foo")
        .build()
        .validate_datadog_trace(
            &mut router,
            Query::builder()
                .traced(true)
                .header("apollographql-client-name", "foo")
                .build(),
        )
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

struct DatadogTraceSpec {
    trace_spec: TraceSpec,
}
impl Deref for DatadogTraceSpec {
    type Target = TraceSpec;

    fn deref(&self) -> &Self::Target {
        &self.trace_spec
    }
}

impl Verifier for DatadogTraceSpec {
    fn spec(&self) -> &TraceSpec {
        &self.trace_spec
    }

    async fn get_trace(&self, trace_id: TraceId) -> Result<Value, BoxError> {
        let datadog_id = trace_id.to_datadog();
        let url = format!("http://localhost:8126/test/traces?trace_ids={datadog_id}");
        println!("url: {}", url);
        let value: serde_json::Value = reqwest::get(url)
            .await
            .map_err(|e| anyhow!("failed to contact datadog; {}", e))?
            .json()
            .await
            .map_err(|e| anyhow!("failed to contact datadog; {}", e))?;
        Ok(value)
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

    fn verify_services(&self, trace: &Value) -> Result<(), BoxError> {
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
                "unexpected traces, got {actual_services:?} expected {expected_services:?}"
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
                    psr,
                    "psr mismatch"
                );
            }
        }
        Ok(())
    }

    fn verify_span_attributes(&self, trace: &Value) -> Result<(), BoxError> {
        for (key, value) in self.attributes.iter() {
            // extracts a list of span attribute values with the provided key
            let binding = trace.select_path(&format!("$..meta..['{key}']"))?;
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

    fn verify_resources(&self, trace: &Value) -> Result<(), BoxError> {
        if !self.trace_spec.resources.is_empty() {
            let spans = trace.select_path("$..[?(@.service=='router')]")?;
            for span in spans {
                for resource in span.select_path("$.meta")? {
                    for (key, value) in &self.trace_spec.resources {
                        let mut found = false;
                        if let Some(resource_value) =
                            resource.as_object().and_then(|resource| resource.get(*key))
                        {
                            let resource_value =
                                resource_value.as_string().expect("resources are strings");
                            if resource_value == *value {
                                found = true;
                            }
                        }
                        if !found {
                            return Err(BoxError::from(format!(
                                "resource not found: {key}={value}",
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl TraceSpec {
    async fn validate_datadog_trace(
        self,
        router: &mut IntegrationTest,
        query: Query,
    ) -> Result<(), BoxError> {
        DatadogTraceSpec { trace_spec: self }
            .validate_trace(router, query)
            .await
    }
}
