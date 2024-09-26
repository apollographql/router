extern crate core;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::anyhow;
use opentelemetry_api::trace::TraceId;
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

use crate::integration::common::graph_os_enabled;
use crate::integration::common::Telemetry;
use crate::integration::IntegrationTest;
use crate::integration::ValueExt;

#[tokio::test(flavor = "multi_thread")]
async fn test_basic() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
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

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    for _ in 0..2 {
        let (id, result) = router.execute_query(&query).await;
        assert!(!result
            .headers()
            .get("apollo-custom-trace-id")
            .unwrap()
            .is_empty());
        Spec::builder()
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
            .build()
            .validate_trace(id, &mock_server)
            .await?;
        Spec::builder()
            .service("router")
            .build()
            .validate_metrics(&mock_server)
            .await?;
        router.touch_config().await;
        router.assert_reloaded().await;
    }
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_datadog_propagator() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_propagation.router.yaml")
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

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, _) = router.execute_query(&query).await;
    Spec::builder()
        .services(["client", "router", "subgraph"].into())
        .priority_sampled("1")
        .build()
        .validate_trace(id, &mock_server)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_request_with_datadog_propagator_no_agent() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
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

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, _) = router.execute_query(&query).await;
    Spec::builder()
        .services(["client", "router", "subgraph"].into())
        .build()
        .validate_trace(id, &mock_server)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_no_sample_datadog_agent() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_agent_no_sample.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder().config(&config).build().await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, _) = router.execute_untraced_query(&query).await;
    Spec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .build()
        .validate_trace(id, &mock_server)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_sample_datadog_agent() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_agent_sample.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder().config(&config).build().await;

    router.start().await;
    router.assert_started().await;

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, _) = router.execute_untraced_query(&query).await;
    Spec::builder()
        .services(["router"].into())
        .priority_sampled("1")
        .build()
        .validate_trace(id, &mock_server)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_untraced_request_sample_datadog_agent_unsampled() -> Result<(), BoxError> {
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_agent_sample_no_sample.router.yaml")
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

    let query = json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}});
    let (id, _) = router.execute_untraced_query(&query).await;
    Spec::builder()
        .services(["router"].into())
        .priority_sampled("0")
        .build()
        .validate_trace(id, &mock_server)
        .await?;
    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_priority_sampling_propagated() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_propagation.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        // We're using datadog propagation as this is what we are trying to test.
        .telemetry(Telemetry::Datadog)
        .config(config)
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Parent based sampling. psr MUST be populated with the value that we pass in.
    test_psr(
        &mut router,
        Some("-1"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("-1")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("0"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("0")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("1"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("2"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("2")
            .build(),
        &mock_server,
    )
    .await?;

    // No psr was passed in the router is free to set it. This will be 1 as we are going to sample here.
    test_psr(
        &mut router,
        None,
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
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
    let mock_server = mock_otlp_server().await;
    let config = include_str!("fixtures/otlp_datadog_propagation_no_parent_sampler.router.yaml")
        .replace("<otel-collector-endpoint>", &mock_server.uri());
    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .config(config)
        .responder(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}),
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // The router will ignore the upstream PSR as parent based sampling is disabled.
    test_psr(
        &mut router,
        Some("-1"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("0"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("1"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;
    test_psr(
        &mut router,
        Some("2"),
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;

    test_psr(
        &mut router,
        None,
        Spec::builder()
            .services(["router"].into())
            .priority_sampled("1")
            .build(),
        &mock_server,
    )
    .await?;

    router.graceful_shutdown().await;

    Ok(())
}

async fn test_psr(
    router: &mut IntegrationTest,
    psr: Option<&str>,
    trace_spec: Spec,
    mock_server: &MockServer,
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
    trace_spec.validate_trace(id, mock_server).await?;
    Ok(())
}

#[derive(buildstructor::Builder)]
struct Spec {
    operation_name: Option<String>,
    version: Option<String>,
    services: HashSet<&'static str>,
    span_names: HashSet<&'static str>,
    measured_spans: HashSet<&'static str>,
    unmeasured_spans: HashSet<&'static str>,
    priority_sampled: Option<&'static str>,
}

impl Spec {
    #[allow(clippy::too_many_arguments)]
    async fn validate_trace(&self, id: TraceId, mock_server: &MockServer) -> Result<(), BoxError> {
        for _ in 0..10 {
            if self.find_valid_trace(id, mock_server).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.find_valid_trace(id, mock_server).await?;
        Ok(())
    }

    async fn validate_metrics(&self, mock_server: &MockServer) -> Result<(), BoxError> {
        for _ in 0..10 {
            if self.find_valid_metrics(mock_server).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.find_valid_metrics(mock_server).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn find_valid_trace(
        &self,
        trace_id: TraceId,
        mock_server: &MockServer,
    ) -> Result<(), BoxError> {
        // A valid trace has:
        // * All three services
        // * The correct spans
        // * All spans are parented
        // * Required attributes of 'router' span has been set

        let requests = mock_server.received_requests().await;
        let trace= Value::Array(requests.unwrap_or_default().iter().filter(|r| r.url.path().ends_with("/traces"))
            .filter_map(|r|{
                match opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest::decode(
                    bytes::Bytes::copy_from_slice(&r.body),
                ) {
                    Ok(trace) => {
                        match serde_json::to_value(trace) {
                            Ok(trace) => {
                                Some(trace) }
                            Err(_) => {
                                None
                            }
                        }
                    }
                    Err(_) => {
                        None
                    }
                }
            }).filter(|t| {

            let datadog_trace_id = TraceId::from_u128(trace_id.to_datadog() as u128);
            let trace_found1 = !t.select_path(&format!("$..[?(@.traceId == '{}')]", trace_id)).unwrap_or_default().is_empty();
            let trace_found2 = !t.select_path(&format!("$..[?(@.traceId == '{}')]", datadog_trace_id)).unwrap_or_default().is_empty();
            trace_found1 | trace_found2
        }).collect());

        self.verify_services(&trace)?;
        self.verify_spans_present(&trace)?;
        self.verify_measured_spans(&trace)?;
        self.verify_operation_name(&trace)?;
        self.verify_priority_sampled(&trace)?;
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
        self.validate_span_kind(trace, "router", "server")?;
        self.validate_span_kind(trace, "supergraph", "internal")?;
        self.validate_span_kind(trace, "http_request", "client")?;
        Ok(())
    }

    fn verify_services(&self, trace: &Value) -> Result<(), BoxError> {
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
                "incomplete traces, got {actual_services:?} expected {expected_services:?}"
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
        let kind = match kind {
            "internal" => 1,
            "client" => 3,
            "server" => 2,
            _ => panic!("unknown kind"),
        };
        let binding1 = trace.select_path(&format!(
            "$..spans..[?(@.kind == {})]..[?(@.key == 'otel.original_name')].value..[?(@ == '{}')]",
            kind, name
        ))?;
        let binding2 = trace.select_path(&format!(
            "$..spans..[?(@.kind == {} && @.name == '{}')]",
            kind, name
        ))?;
        let binding = binding1.first().or(binding2.first());

        if binding.is_none() {
            return Err(BoxError::from(format!(
                "span.kind missing or incorrect {}, {}",
                name, kind
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
                assert_eq!(
                    sampling_priority
                        .as_i64()
                        .expect("psr not an integer")
                        .to_string(),
                    psr
                );
            }
        } else {
            assert!(trace.select_path("$..[?(@.name == 'execution')]..[?(@.key == 'sampling.priority')].value.intValue")?.is_empty())
        }
        Ok(())
    }

    async fn find_valid_metrics(&self, mock_server: &MockServer) -> Result<(), BoxError> {
        let requests = mock_server
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
}

async fn mock_otlp_server() -> MockServer {
    let mock_server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
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
    mock_server
}

pub(crate) trait DatadogId {
    fn to_datadog(&self) -> u64;
}
impl DatadogId for TraceId {
    fn to_datadog(&self) -> u64 {
        let bytes = &self.to_bytes()[std::mem::size_of::<u64>()..std::mem::size_of::<u128>()];
        u64::from_be_bytes(bytes.try_into().unwrap())
    }
}
