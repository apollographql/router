use std::collections::HashMap;

use tower::BoxError;

use super::mock_otlp_server;
use super::mock_otlp_server_delayed;
use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;
use crate::integration::telemetry::TraceSpec;

#[tokio::test(flavor = "multi_thread")]
async fn test_trace_error() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server_delayed().await;
    let config = include_str!("../fixtures/otlp_invalid_endpoint.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp.router.yaml")
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
    let config = include_str!("../fixtures/otlp.router.yaml")
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
async fn test_otlp_tracing_reload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(0..).await;
    let config_initial = include_str!("../fixtures/otlp_tracing.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server.uri())),
        })
        .config(&config_initial)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Verify initial resource env=local1
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .resource("env", "local1")
        .build()
        .validate_otlp_trace(&mut router, &mock_server, Query::default())
        .await?;

    // This config will NOT reload tracing as the config did not change
    router.update_config(&config_initial).await;
    router.assert_reloaded().await;
    router.assert_log_not_contained("OpenTelemetry trace error occurred");

    // Execute query and verify resource is still local1
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .resource("env", "local1")
        .build()
        .validate_otlp_trace(&mut router, &mock_server, Query::default())
        .await?;

    // This config will force a reload as it changes the resource env value
    let config_reload = include_str!("../fixtures/otlp_tracing_reload.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    router.update_config(&config_reload).await;
    router.assert_reloaded().await;

    // Execute query and verify resource changed to local2
    TraceSpec::builder()
        .services(["client", "router", "subgraph"].into())
        .resource("env", "local2")
        .build()
        .validate_otlp_trace(&mut router, &mock_server, Query::default())
        .await?;

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_datadog_propagator() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_propagation.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_propagation_no_agent.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config =
        include_str!("../fixtures/otlp_datadog_request_with_zipkin_propagator.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_agent_no_sample.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_agent_sample.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_agent_sample_no_sample.router.yaml")
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
        panic!("Error: test skipped because GraphOS is not enabled");
    }
    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("../fixtures/otlp_datadog_propagation.router.yaml")
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
        "../fixtures/otlp_parent_sampler_very_small_no_parent_no_agent_sampling.router.yaml"
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
    let config = include_str!("../fixtures/otlp_datadog_propagation_no_parent_sampler.router.yaml")
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
    let config = include_str!("../fixtures/otlp.router.yaml")
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
    let config = include_str!("../fixtures/otlp_override_client_name.router.yaml")
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
