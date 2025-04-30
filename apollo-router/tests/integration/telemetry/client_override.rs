use std::collections::HashMap;

use super::otlp::mock_otlp_server;
use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;
use crate::integration::telemetry::TraceSpec;

const HEADER_VALUES: [Option<&'static str>; 4] = [None, Some(""), Some("foo"), Some("bar")];

#[tokio::test(flavor = "multi_thread")]
async fn test_client_name_is_included_in_telemetry() {
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

    for header_value in HEADER_VALUES {
        let mut headers = HashMap::default();
        if let Some(value) = header_value {
            headers.insert("apollographql-client-name".to_string(), value.to_string());
        }

        let trace_spec = TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .attribute("client.name", header_value.unwrap_or(""))
            .build();
        let query = Query::builder().traced(true).headers(headers).build();
        trace_spec
            .validate_otlp_trace(&mut router, &mock_server, query)
            .await
            .expect(&format!("Failed with header value {header_value:?}"));
    }

    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_plugin_overridden_client_name_is_included_in_telemetry() {
    if !graph_os_enabled() {
        panic!("Error: test skipped because GraphOS is not enabled");
    }

    let mock_server = mock_otlp_server(1..).await;
    let config = include_str!("fixtures/override_client_name.router.yaml")
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

    // rhai script overrides client.name - no matter what client name we pass via headers, it should
    // end up equalling the value set in the script (`foo`)
    for header_value in HEADER_VALUES {
        let mut headers = HashMap::default();
        if let Some(value) = header_value {
            headers.insert("apollographql-client-name".to_string(), value.to_string());
        }

        let trace_spec = TraceSpec::builder()
            .services(["client", "router", "subgraph"].into())
            .attribute("client.name", "foo")
            .build();
        let query = Query::builder().traced(true).headers(headers).build();
        trace_spec
            .validate_otlp_trace(&mut router, &mock_server, query)
            .await
            .expect(&format!("Failed with header value {header_value:?}"));
    }

    router.graceful_shutdown().await;
}
