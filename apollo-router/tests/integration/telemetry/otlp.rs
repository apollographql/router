#![cfg(all(target_os = "linux", target_arch = "x86_64", test))]
extern crate core;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use itertools::Itertools;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceResponse;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use crate::integration::common::Telemetry;
use crate::integration::common::ValueExt;
use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    let mock_server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/traces"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            ExportTraceServiceResponse::default().encode_to_vec(),
            "application/x-protobuf",
        ))
        .expect(1..)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            ExportMetricsServiceResponse::default().encode_to_vec(),
            "application/x-protobuf",
        ))
        .expect(1..)
        .mount(&mock_server)
        .await;

    let config = include_str!("fixtures/otlp.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp {
            endpoint: format!("{}/traces", mock_server.uri()),
        })
        .config(&config)
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
        validate_telemetry(
            &mock_server,
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

async fn validate_telemetry(
    mock_server: &MockServer,
    _id: String,
    query: &Value,
    operation_name: Option<&str>,
    services: &[&'static str],
    custom_span_instrumentation: bool,
) -> Result<(), BoxError> {
    for _ in 0..10 {
        let trace_valid = find_valid_trace(
            mock_server,
            query,
            operation_name,
            services,
            custom_span_instrumentation,
        )
        .await;

        let metrics_valid = find_valid_metrics(mock_server, query, operation_name, services).await;

        if metrics_valid.is_ok() && trace_valid.is_ok() {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    find_valid_trace(
        mock_server,
        query,
        operation_name,
        services,
        custom_span_instrumentation,
    )
    .await?;
    find_valid_metrics(mock_server, query, operation_name, services).await?;

    Ok(())
}

async fn find_valid_trace(
    mock_server: &MockServer,
    _query: &Value,
    _operation_name: Option<&str>,
    services: &[&'static str],
    _custom_span_instrumentation: bool,
) -> Result<(), BoxError> {
    let requests = mock_server
        .received_requests()
        .await
        .expect("Could not get otlp requests");

    // A valid trace has:
    // * A valid service name
    // * All three services
    // * The correct spans
    // * All spans are parented
    // * Required attributes of 'router' span has been set
    let traces: Vec<_>= requests
        .iter()
        .filter_map(|r| {
            if r.url.path().ends_with("/traces") {
                match opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest::decode(
                    bytes::Bytes::copy_from_slice(&r.body),
                ) {
                    Ok(trace) => {
                        match serde_json::to_value(trace) {
                            Ok(trace) => { Some(Ok(trace)) }
                            Err(e) => {
                                Some(Err(BoxError::from(format!("failed to decode trace: {}", e))))
                            }
                        }
                    }
                    Err(e) => {
                        Some(Err(BoxError::from(format!("failed to decode trace: {}", e))))
                    }
                }
            }
            else {
                None
            }
        })
        .try_collect()?;
    if !traces.is_empty() {
        let json_trace = serde_json::Value::Array(traces);
        verify_trace_participants(&json_trace, services)?;

        Ok(())
    } else {
        Err(anyhow!("No traces received").into())
    }
}

fn verify_trace_participants(trace: &Value, services: &[&'static str]) -> Result<(), BoxError> {
    let actual_services: HashSet<String> = trace
        .select_path("$..resource.attributes[?(@.key=='service.name')].value.stringValue")?
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

fn validate_service_name(trace: Value) -> Result<(), BoxError> {
    let service_name =
        trace.select_path("$..resource.attributes[?(@.key=='service.name')].value.stringValue")?;
    assert_eq!(
        service_name.first(),
        Some(&&Value::String("router".to_string()))
    );
    Ok(())
}

async fn find_valid_metrics(
    mock_server: &MockServer,
    _query: &Value,
    _operation_name: Option<&str>,
    _services: &[&'static str],
) -> Result<(), BoxError> {
    let requests = mock_server
        .received_requests()
        .await
        .expect("Could not get otlp requests");
    if let Some(metrics) = requests.iter().find(|r| r.url.path().ends_with("/metrics")) {
        let metrics = opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest::decode(bytes::Bytes::copy_from_slice(&metrics.body))?;
        let json_trace = serde_json::to_value(metrics)?;
        // For now just validate service name.
        validate_service_name(json_trace)?;

        Ok(())
    } else {
        Err(anyhow!("No metrics received").into())
    }
}
