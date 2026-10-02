use derivative::Derivative;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry::trace::Event;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::Status;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::WithHttpConfig;
use opentelemetry_otlp::WithTonicConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanEvents;
use opentelemetry_sdk::trace::SpanExporter;
use opentelemetry_sdk::trace::SpanLinks;
use sys_info::hostname;
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;
use tonic::transport::ClientTlsConfig;
use tower::BoxError;
use url::Url;

use super::apollo::ErrorsConfiguration;
use super::config_new::subgraph::attributes::SUBGRAPH_NAME;
use super::otlp::Protocol;
use super::otlp::TelemetryDataKind;
use super::otlp::process_endpoint;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
use super::tracing::apollo_telemetry::LightSpanData;
use super::tracing::apollo_telemetry::encode_ftv1_trace;
use super::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
use super::tracing::apollo_telemetry::extract_string;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::apollo::router_id;
use crate::plugins::telemetry::apollo_exporter::ROUTER_REPORT_TYPE_TRACES;
use crate::plugins::telemetry::apollo_exporter::ROUTER_TRACING_PROTOCOL_OTLP;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;

/// The Apollo Otlp exporter is a thin wrapper around the OTLP SpanExporter.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct ApolloOtlpExporter {
    instrumentation_scope: InstrumentationScope,
    #[derivative(Debug = "ignore")]
    otlp_exporter: opentelemetry_otlp::SpanExporter,
    errors_configuration: ErrorsConfiguration,
}

impl ApolloOtlpExporter {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        endpoint: &Url,
        protocol: &Protocol,
        batch_config: &BatchProcessorConfig,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
        errors_configuration: &ErrorsConfiguration,
    ) -> Result<ApolloOtlpExporter, BoxError> {
        tracing::debug!(endpoint = %endpoint, "creating Apollo OTLP traces exporter");

        let mut otlp_exporter = match protocol {
            Protocol::Grpc => {
                let mut metadata = MetadataMap::new();
                metadata.insert("apollo.api.key", MetadataValue::try_from(apollo_key)?);
                SpanExporterBuilder::new()
                    .with_tonic()
                    .with_tls_config(ClientTlsConfig::new().with_native_roots())
                    .with_timeout(batch_config.max_export_timeout)
                    .with_endpoint(endpoint.to_string())
                    .with_metadata(metadata)
                    .with_compression(opentelemetry_otlp::Compression::Gzip)
                    .build()?
            }
            Protocol::Http => {
                let endpoint_str = process_endpoint(
                    &Some(endpoint.to_string()),
                    &TelemetryDataKind::Traces,
                    &Protocol::Http,
                )?
                .ok_or("A valid HTTP OTLP endpoint is required when using the HTTP protocol")?;
                let mut headers = std::collections::HashMap::new();
                headers.insert("x-api-key".to_string(), apollo_key.to_string());
                SpanExporterBuilder::new()
                    .with_http()
                    .with_timeout(batch_config.max_export_timeout)
                    .with_compression(opentelemetry_otlp::Compression::Gzip)
                    .with_headers(headers)
                    .with_endpoint(endpoint_str)
                    .build()?
            }
        };

        otlp_exporter.set_resource(
            &Resource::builder_empty()
                .with_attributes([
                    KeyValue::new("apollo.router.id", router_id()),
                    KeyValue::new("apollo.graph.ref", apollo_graph_ref.to_string()),
                    KeyValue::new("apollo.schema.id", schema_id.to_string()),
                    KeyValue::new(
                        "apollo.user.agent",
                        format!(
                            "{}@{}",
                            std::env!("CARGO_PKG_NAME"),
                            std::env!("CARGO_PKG_VERSION")
                        ),
                    ),
                    KeyValue::new("apollo.client.host", hostname()?),
                    KeyValue::new("apollo.client.uname", get_uname()?),
                ])
                .build(),
        );

        Ok(Self {
            instrumentation_scope: InstrumentationScope::builder(GLOBAL_TRACER_NAME)
                .with_version(format!(
                    "{}@{}",
                    std::env!("CARGO_PKG_NAME"),
                    std::env!("CARGO_PKG_VERSION")
                ))
                .build(),
            otlp_exporter,
            errors_configuration: errors_configuration.clone(),
        })
    }

    pub(crate) fn prepare_for_export(&self, trace_spans: Vec<LightSpanData>) -> Vec<SpanData> {
        let mut export_spans: Vec<SpanData> = Vec::new();

        trace_spans.into_iter().for_each(|span| {
            tracing::debug!("apollo otlp: preparing span '{}'", span.name);
            match span.name.as_ref() {
                SUPERGRAPH_SPAN_NAME => {
                    if span
                        .attributes
                        .contains_key(&APOLLO_PRIVATE_OPERATION_SIGNATURE)
                    {
                        export_spans.push(self.base_prepare_span(span));
                    }
                }
                SUBGRAPH_SPAN_NAME => export_spans.push(self.prepare_subgraph_span(span)),
                _ => export_spans.push(self.base_prepare_span(span)),
            };
        });

        export_spans
    }

    fn extract_span_events(span: &LightSpanData) -> SpanEvents {
        let mut span_events = SpanEvents::default();
        for light_event in &span.events {
            span_events.events.push(Event::new(
                light_event.name.clone(),
                light_event.timestamp,
                light_event
                    .attributes
                    .iter()
                    .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
                    .collect(),
                0,
            ));
        }
        span_events
    }

    fn base_prepare_span(&self, span: LightSpanData) -> SpanData {
        SpanData {
            span_context: SpanContext::new(
                span.trace_id,
                span.span_id,
                TraceFlags::default().with_sampled(true),
                true,
                TraceState::default(),
            ),
            parent_span_id: span.parent_span_id,
            parent_span_is_remote: false,
            span_kind: span.span_kind.clone(),
            name: span.name.clone(),
            start_time: span.start_time,
            end_time: span.end_time,
            attributes: span
                .attributes
                .iter()
                .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
                .collect(),
            events: Self::extract_span_events(&span),
            links: SpanLinks::default(),
            status: span.status,
            instrumentation_scope: self.instrumentation_scope.clone(),
            dropped_attributes_count: span.droppped_attribute_count,
        }
    }

    /// Parses and redacts errors from ftv1 traces.
    /// Sets the span status to error if there are any errors.
    fn prepare_subgraph_span(&self, mut span: LightSpanData) -> SpanData {
        let mut status = Status::Unset;

        // If there is an FTV1 attribute, process it for error redaction and replace it
        if let Some(ftv1) = span.attributes.get(&APOLLO_PRIVATE_FTV1) {
            let subgraph_name = span
                .attributes
                .get(&SUBGRAPH_NAME)
                .and_then(extract_string)
                .unwrap_or_default();
            let subgraph_error_config = self
                .errors_configuration
                .subgraph
                .get_error_config(&subgraph_name);
            if let Some(Ok((trace_result, error_count))) =
                extract_ftv1_trace_with_error_count(ftv1, subgraph_error_config)
            {
                if error_count > 0 {
                    status = Status::error("ftv1")
                }
                let encoded = encode_ftv1_trace(&trace_result);
                span.attributes.insert(APOLLO_PRIVATE_FTV1, encoded.into());
            }
        }

        SpanData {
            span_context: SpanContext::new(
                span.trace_id,
                span.span_id,
                TraceFlags::default().with_sampled(true),
                true,
                TraceState::default(),
            ),
            parent_span_id: span.parent_span_id,
            parent_span_is_remote: false,
            span_kind: span.span_kind.clone(),
            name: span.name.clone(),
            start_time: span.start_time,
            end_time: span.end_time,
            attributes: span
                .attributes
                .iter()
                .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
                .collect(),
            events: Self::extract_span_events(&span),
            links: SpanLinks::default(),
            status,
            instrumentation_scope: self.instrumentation_scope.clone(),
            dropped_attributes_count: span.droppped_attribute_count,
        }
    }
}

impl SpanExporter for ApolloOtlpExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        self.otlp_exporter.export(batch).await?;
        // re-use the metric we already have in apollo_exporter but attach the protocol
        u64_counter!(
            "apollo.router.telemetry.studio.reports",
            "The number of reports submitted to Studio by the Router",
            1,
            report.type = ROUTER_REPORT_TYPE_TRACES,
            report.protocol = ROUTER_TRACING_PROTOCOL_OTLP
        );
        Ok(())
    }

    fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> OTelSdkResult {
        self.otlp_exporter.shutdown_with_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::SystemTime;

    use opentelemetry::Key;
    use opentelemetry::Value;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::Status;
    use opentelemetry::trace::TraceId;
    use url::Url;

    use super::ApolloOtlpExporter;
    use crate::plugins::telemetry::apollo::ErrorsConfiguration;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Error;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Node;
    use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
    use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
    use crate::plugins::telemetry::otlp::Protocol;
    use crate::plugins::telemetry::tracing::BatchProcessorConfig;
    use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
    use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
    use crate::plugins::telemetry::tracing::apollo_telemetry::LightSpanData;
    use crate::plugins::telemetry::tracing::apollo_telemetry::decode_ftv1_trace;
    use crate::plugins::telemetry::tracing::apollo_telemetry::encode_ftv1_trace;

    fn test_exporter(errors_configuration: ErrorsConfiguration) -> ApolloOtlpExporter {
        // The builder performs no network I/O, so a dummy endpoint is fine for exercising
        // the span-preparation logic (`prepare_for_export` / `prepare_subgraph_span`).
        ApolloOtlpExporter::new(
            &Url::parse("https://example.com:4317").unwrap(),
            &Protocol::Grpc,
            &BatchProcessorConfig::default(),
            "test-key",
            "test-graph@current",
            "test-schema-id",
            &errors_configuration,
        )
        .expect("could not build test exporter")
    }

    fn light_span(name: &'static str, attributes: HashMap<Key, Value>) -> LightSpanData {
        LightSpanData {
            trace_id: TraceId::from(1u128),
            span_id: SpanId::from(1u64),
            parent_span_id: SpanId::INVALID,
            span_kind: SpanKind::Internal,
            name: name.into(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: SystemTime::UNIX_EPOCH,
            attributes,
            status: Status::Unset,
            droppped_attribute_count: 0,
            events: Vec::new(),
        }
    }

    fn ftv1_attribute_with_error() -> (Key, Value) {
        let trace = Trace {
            root: Some(Node {
                error: vec![Error {
                    message: "boom".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"extensions":{"code":"OOPS"}}"#),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        (
            APOLLO_PRIVATE_FTV1,
            Value::String(encode_ftv1_trace(&trace).into()),
        )
    }

    #[tokio::test]
    async fn keeps_trace_with_operation_signature() {
        let exporter = test_exporter(ErrorsConfiguration::default());
        let mut attributes = HashMap::new();
        attributes.insert(
            APOLLO_PRIVATE_OPERATION_SIGNATURE,
            Value::String("query { __typename }".into()),
        );
        let spans = vec![light_span(SUPERGRAPH_SPAN_NAME, attributes)];

        let prepared = exporter.prepare_for_export(spans);
        assert_eq!(prepared.len(), 1);
    }

    /// A subgraph span whose ftv1 trace contains errors is marked as errored, and the
    /// (redacted) trace is re-encoded back into the ftv1 attribute.
    #[tokio::test]
    async fn subgraph_span_with_ftv1_errors_sets_error_status_and_reencodes() {
        let exporter = test_exporter(ErrorsConfiguration::default());
        let mut attributes = HashMap::new();
        let (ftv1_key, ftv1_value) = ftv1_attribute_with_error();
        attributes.insert(ftv1_key, ftv1_value);
        let span = light_span(SUBGRAPH_SPAN_NAME, attributes);

        let prepared = exporter.prepare_subgraph_span(span);

        assert_eq!(prepared.status, Status::error("ftv1"));

        let reencoded = prepared
            .attributes
            .iter()
            .find(|kv| kv.key == APOLLO_PRIVATE_FTV1)
            .and_then(|kv| match &kv.value {
                Value::String(s) => decode_ftv1_trace(s.as_str()),
                _ => None,
            })
            .expect("ftv1 attribute must be present and decodable after re-encode");
        // Default config redacts errors, so the re-encoded trace proves both that the
        // decode/re-encode round-trip ran and that redaction was applied.
        assert_eq!(reencoded.root.unwrap().error[0].message, "<redacted>");
    }

    #[tokio::test]
    async fn subgraph_span_without_ftv1_is_unset() {
        let exporter = test_exporter(ErrorsConfiguration::default());
        let span = light_span(SUBGRAPH_SPAN_NAME, HashMap::new());

        let prepared = exporter.prepare_subgraph_span(span);

        assert_eq!(prepared.status, Status::Unset);
    }
}
