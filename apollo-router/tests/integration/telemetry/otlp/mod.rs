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
use crate::integration::telemetry::DatadogId;
use crate::integration::telemetry::TraceSpec;
use crate::integration::telemetry::verifier::Verifier;

mod metrics;
mod tracing;

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
        ::tracing::debug!("found services {:?}", actual_services);
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
        ::tracing::debug!("found spans {:?}", operation_names);
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

pub(crate) fn find_metric_in_request<'a>(
    name: &str,
    request: &'a opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest,
) -> Option<&'a opentelemetry_proto::tonic::metrics::v1::Metric> {
    request
        .resource_metrics
        .iter()
        .flat_map(|rm| &rm.scope_metrics)
        .flat_map(|sm| &sm.metrics)
        .find(|m| m.name == name)
}

pub(crate) async fn mock_otlp_server_delayed() -> MockServer {
    let mock_server = MockServer::start().await;
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

pub(crate) async fn mock_otlp_server<T: Into<Times> + Clone>(expected_requests: T) -> MockServer {
    let mock_server = MockServer::start().await;
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
    pub(crate) async fn validate_otlp_trace(
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
    pub(crate) async fn validate_otlp_metrics(
        self,
        mock_server: &MockServer,
    ) -> Result<(), BoxError> {
        OtlpTraceSpec {
            trace_spec: self,
            mock_server,
        }
        .validate_metrics()
        .await
    }
}
