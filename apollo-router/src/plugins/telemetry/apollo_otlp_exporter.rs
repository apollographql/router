use derivative::Derivative;
use futures::TryFutureExt;
use futures::future;
use futures::future::BoxFuture;
use opentelemetry::InstrumentationLibrary;
use opentelemetry::KeyValue;
use opentelemetry::trace::Event;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::Status;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::export::trace::ExportResult;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::export::trace::SpanExporter;
use opentelemetry_sdk::trace::SpanEvents;
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
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
use super::tracing::apollo_telemetry::LightSpanData;
use super::tracing::apollo_telemetry::encode_ftv1_trace;
use super::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
use super::tracing::apollo_telemetry::extract_string;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::apollo::router_id;
use crate::plugins::telemetry::apollo_exporter::ROUTER_REPORT_TYPE_TRACES;
use crate::plugins::telemetry::apollo_exporter::ROUTER_TRACING_PROTOCOL_OTLP;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::consts::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;

/// The Apollo Otlp exporter is a thin wrapper around the OTLP SpanExporter.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct ApolloOtlpExporter {
    exporter_config: BatchProcessorConfig,
    endpoint: Url,
    apollo_key: String,
    intrumentation_library: InstrumentationLibrary,
    #[derivative(Debug = "ignore")]
    otlp_exporter: opentelemetry_otlp::SpanExporter,
    errors_configuration: ErrorsConfiguration,
}

impl ApolloOtlpExporter {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        endpoint: &Url,
        protocol: &Protocol,
        exporter_config: &BatchProcessorConfig,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
        errors_configuration: &ErrorsConfiguration,
    ) -> Result<ApolloOtlpExporter, BoxError> {
        tracing::debug!(endpoint = %endpoint, "creating Apollo OTLP traces exporter");

        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", MetadataValue::try_from(apollo_key)?);
        let mut otlp_exporter = match protocol {
            Protocol::Grpc => SpanExporterBuilder::from(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_tls_config(ClientTlsConfig::new().with_native_roots())
                    .with_timeout(exporter_config.max_export_timeout)
                    .with_endpoint(endpoint.to_string())
                    .with_metadata(metadata)
                    .with_compression(opentelemetry_otlp::Compression::Gzip),
            )
            .build_span_exporter()?,
            // So far only using HTTP path for testing - the Studio backend only accepts GRPC today.
            Protocol::Http => SpanExporterBuilder::from(
                opentelemetry_otlp::new_exporter()
                    .http()
                    .with_timeout(exporter_config.max_export_timeout)
                    .with_endpoint(endpoint.to_string()),
            )
            .build_span_exporter()?,
        };

        otlp_exporter.set_resource(&Resource::new([
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
        ]));

        Ok(Self {
            endpoint: endpoint.clone(),
            exporter_config: exporter_config.clone(),
            apollo_key: apollo_key.to_string(),
            intrumentation_library: InstrumentationLibrary::builder(GLOBAL_TRACER_NAME)
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

    pub(crate) fn prepare_for_export(
        &self,
        trace_spans: Vec<LightSpanData>,
    ) -> Option<Vec<SpanData>> {
        let mut export_spans: Vec<SpanData> = Vec::new();
        let mut send_trace: bool = false;

        trace_spans.into_iter().for_each(|span| {
            tracing::debug!("apollo otlp: preparing span '{}'", span.name);
            match span.name.as_ref() {
                SUPERGRAPH_SPAN_NAME => {
                    if span
                        .attributes
                        .contains_key(&APOLLO_PRIVATE_OPERATION_SIGNATURE)
                    {
                        export_spans.push(self.base_prepare_span(span));
                        // Mirrors the existing implementation in apollo_telemetry
                        // which filters out traces that are missing the signature attribute.
                        // In practice, this results in excluding introspection queries.
                        send_trace = true
                    }
                }
                SUBGRAPH_SPAN_NAME => export_spans.push(self.prepare_subgraph_span(span)),
                _ => export_spans.push(self.base_prepare_span(span)),
            };
        });
        if send_trace {
            tracing::debug!("apollo otlp: sending trace");
            Some(export_spans)
        } else {
            tracing::debug!("apollo otlp: dropping trace");
            None
        }
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
            instrumentation_lib: self.intrumentation_library.clone(),
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
            instrumentation_lib: self.intrumentation_library.clone(),
            dropped_attributes_count: span.droppped_attribute_count,
        }
    }

    pub(crate) fn export(&mut self, spans: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let fut = self.otlp_exporter.export(spans);
        Box::pin(fut.and_then(|_| {
            // re-use the metric we already have in apollo_exporter but attach the protocol
            u64_counter!(
                "apollo.router.telemetry.studio.reports",
                "The number of reports submitted to Studio by the Router",
                1,
                report.type = ROUTER_REPORT_TYPE_TRACES,
                report.protocol = ROUTER_TRACING_PROTOCOL_OTLP
            );
            future::ready(Ok(()))
        }))
    }

    pub(crate) fn shutdown(&mut self) {
        self.otlp_exporter.shutdown()
    }
}
