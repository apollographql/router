use std::borrow::Cow;
use std::sync::Arc;

use derivative::Derivative;
use futures::future;
use futures::future::BoxFuture;
use futures::TryFutureExt;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::sdk::trace::EvictedQueue;
use opentelemetry::sdk::Resource;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::Status;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::InstrumentationLibrary;
use opentelemetry::KeyValue;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use parking_lot::Mutex;
use sys_info::hostname;
use tonic::codec::CompressionEncoding;
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;
use tower::BoxError;
use url::Url;

use super::apollo::ErrorsConfiguration;
use super::config_new::attributes::SUBGRAPH_NAME;
use super::otlp::Protocol;
use super::tracing::apollo_telemetry::encode_ftv1_trace;
use super::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
use super::tracing::apollo_telemetry::extract_string;
use super::tracing::apollo_telemetry::LightSpanData;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
use crate::plugins::telemetry::apollo::router_id;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::apollo_exporter::ROUTER_REPORT_TYPE_TRACES;
use crate::plugins::telemetry::apollo_exporter::ROUTER_TRACING_PROTOCOL_OTLP;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;

/// The Apollo Otlp exporter is a thin wrapper around the OTLP SpanExporter.
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub(crate) struct ApolloOtlpExporter {
    batch_config: BatchProcessorConfig,
    endpoint: Url,
    apollo_key: String,
    resource_template: Resource,
    intrumentation_library: InstrumentationLibrary,
    #[derivative(Debug = "ignore")]
    otlp_exporter: Arc<Mutex<opentelemetry_otlp::SpanExporter>>,
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

        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", MetadataValue::try_from(apollo_key)?);
        let otlp_exporter = match protocol {
            Protocol::Grpc => {
                let mut span_exporter = SpanExporterBuilder::from(
                    opentelemetry_otlp::new_exporter()
                        .tonic()
                        .with_timeout(batch_config.max_export_timeout)
                        .with_endpoint(endpoint.to_string())
                        .with_metadata(metadata)
                        .with_compression(opentelemetry_otlp::Compression::Gzip),
                )
                .build_span_exporter()?;

                // This is a hack and won't be needed anymore once opentelemetry_otlp will be upgraded
                span_exporter = if let opentelemetry_otlp::SpanExporter::Tonic {
                    trace_exporter,
                    metadata,
                    timeout,
                } = span_exporter
                {
                    opentelemetry_otlp::SpanExporter::Tonic {
                        timeout,
                        metadata,
                        trace_exporter: trace_exporter.accept_compressed(CompressionEncoding::Gzip),
                    }
                } else {
                    span_exporter
                };

                Arc::new(Mutex::new(span_exporter))
            }
            // So far only using HTTP path for testing - the Studio backend only accepts GRPC today.
            Protocol::Http => Arc::new(Mutex::new(
                SpanExporterBuilder::from(
                    opentelemetry_otlp::new_exporter()
                        .http()
                        .with_timeout(batch_config.max_export_timeout)
                        .with_endpoint(endpoint.to_string()),
                )
                .build_span_exporter()?,
            )),
        };

        Ok(Self {
            endpoint: endpoint.clone(),
            batch_config: batch_config.clone(),
            apollo_key: apollo_key.to_string(),
            resource_template: Resource::new([
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
            ]),
            intrumentation_library: InstrumentationLibrary::new(
                GLOBAL_TRACER_NAME,
                Some(format!(
                    "{}@{}",
                    std::env!("CARGO_PKG_NAME"),
                    std::env!("CARGO_PKG_VERSION")
                )),
                Option::<String>::None,
                None,
            ),
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
                        .get(&APOLLO_PRIVATE_OPERATION_SIGNATURE)
                        .is_some()
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
            attributes: span.attributes,
            events: EvictedQueue::new(0),
            links: EvictedQueue::new(0),
            status: span.status,
            // If the underlying exporter supported it, we could
            // group by resource attributes here and significantly reduce the
            // duplicate resource / scope data that will get sent on every span.
            resource: Cow::Owned(self.resource_template.to_owned()),
            instrumentation_lib: self.intrumentation_library.clone(),
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
                span.attributes
                    .insert(KeyValue::new(APOLLO_PRIVATE_FTV1, encoded));
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
            attributes: span.attributes,
            events: EvictedQueue::new(0),
            links: EvictedQueue::new(0),
            status,
            resource: Cow::Owned(self.resource_template.to_owned()),
            instrumentation_lib: self.intrumentation_library.clone(),
        }
    }

    pub(crate) fn export(&self, spans: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let mut exporter = self.otlp_exporter.lock();
        let fut = exporter.export(spans);
        drop(exporter);
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

    pub(crate) fn shutdown(&self) {
        let mut exporter = self.otlp_exporter.lock();
        exporter.shutdown()
    }
}
