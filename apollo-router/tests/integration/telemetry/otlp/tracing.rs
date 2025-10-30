use std::collections::HashMap;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use tower::BoxError;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

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
    let mock_server_uri = "http://will-never-be-used.local";
    let config =
        include_str!("../fixtures/otlp_datadog_request_with_zipkin_propagator.router.yaml")
            .replace("<otel-collector-endpoint>", mock_server_uri);
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", mock_server_uri)),
        })
        .extra_propagator(Telemetry::Datadog)
        .config(&config)
        .build()
        .await;

    router.start().await;
    router.wait_for_log_message("could not create router: datadog propagation cannot be used with any other propagator except for baggage").await;

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

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_ipv6() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    // Create a TCP listener bound to IPv6 localhost
    // Skip test if IPv6 is not available (common in CI environments)
    let listener = match std::net::TcpListener::bind("[::1]:0") {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
            eprintln!("Skipping test_otlp_ipv6: IPv6 not available");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let ipv6_address = listener.local_addr().expect("Failed to get local address");
    let ipv6_endpoint = format!("http://{}", ipv6_address);

    // Create mock server using the IPv6 listener
    let mock_server = wiremock::MockServer::builder()
        .listener(listener)
        .start()
        .await;

    // Set up the expected mocks for traces and metrics
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            ExportTraceServiceResponse::default().encode_to_vec(),
            "application/x-protobuf",
        ))
        .expect(1..)
        .mount(&mock_server)
        .await;

    let config = include_str!("../fixtures/otlp.router.yaml")
        .replace("<otel-collector-endpoint>", &ipv6_endpoint);

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: Some(format!("{}/v1/traces", ipv6_endpoint)),
        })
        .config(&config)
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

    router.graceful_shutdown().await;
    Ok(())
}
