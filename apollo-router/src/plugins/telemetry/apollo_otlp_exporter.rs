use std::borrow::Cow;
use std::sync::Arc;

use derivative::Derivative;
use futures::future::BoxFuture;
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
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;
use tower::BoxError;
use url::Url;
use uuid::Uuid;

use super::apollo::ErrorsConfiguration;
use super::config_new::attributes::SUBGRAPH_NAME;
use super::otlp::Protocol;
use super::tracing::apollo_telemetry::encode_ftv1_trace;
use super::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
use super::tracing::apollo_telemetry::extract_string;
use super::tracing::apollo_telemetry::LightSpanData;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
use super::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::apollo::ROUTER_ID;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;

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
}

impl ApolloOtlpExporter {
    pub(crate) fn new(
        endpoint: &Url,
        protocol: &Protocol,
        batch_config: &BatchProcessorConfig,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloOtlpExporter, BoxError> {
        tracing::debug!(endpoint = %endpoint, "creating Apollo OTLP traces exporter");

        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", MetadataValue::try_from(apollo_key)?);
        let otlp_exporter = match protocol {
            Protocol::Grpc => Arc::new(Mutex::new(
                SpanExporterBuilder::from(
                    opentelemetry_otlp::new_exporter()
                        .tonic()
                        .with_timeout(batch_config.max_export_timeout)
                        .with_endpoint(endpoint.to_string())
                        .with_metadata(metadata),
                    // TBD(tim): figure out why compression seems to be turned off on our collector
                    // .with_compression(opentelemetry_otlp::Compression::Gzip),
                )
                // TBD(tim): do we need another batch processor for this?
                // Seems like we've already set up a batcher earlier in the pipe but not quite sure.
                .build_span_exporter()?,
            )),
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

        return Ok(Self {
            endpoint: endpoint.clone(),
            batch_config: batch_config.clone(),
            apollo_key: apollo_key.to_string(),
            resource_template: Resource::new([
                KeyValue::new(
                    "apollo.router.id",
                    ROUTER_ID.get_or_init(Uuid::new_v4).to_string(),
                ),
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
        });
    }

    pub(crate) fn prepare_for_export(&self, span: LightSpanData, errors_config: &ErrorsConfiguration) -> SpanData {
        match span.name.as_ref() {
            SUBGRAPH_SPAN_NAME => {
                self.prepare_subgraph_span(span, &errors_config)
            },
            _ => SpanData {
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
                attributes: span.attributes.clone(),
                events: EvictedQueue::new(0),
                links: EvictedQueue::new(0),
                status: Status::Unset,
                resource: Cow::Owned(self.resource_template.to_owned()),
                instrumentation_lib: self.intrumentation_library.clone(),
            }
        }
    }

    fn prepare_subgraph_span(&self, span: LightSpanData, errors_config: &ErrorsConfiguration) -> SpanData {
        let mut new_attrs = span.attributes.clone();
        let mut status = Status::Unset;

        // If there is an FTV1 attribute, process it for error redaction and replace it
        if let Some(ftv1) = new_attrs.get(&APOLLO_PRIVATE_FTV1) {
            let subgraph_name = span
            .attributes
            .get(&SUBGRAPH_NAME)
            .and_then(extract_string)
            .unwrap_or_default();
            let subgraph_error_config = errors_config
                .subgraph
                .get_error_config(&subgraph_name);
            if let Some(trace) = extract_ftv1_trace_with_error_count(ftv1, &subgraph_error_config) {
                if let Ok((trace_result, error_count)) = trace {
                    if error_count > 0 {
                        status = Status::error("ftv1")
                    }
                    let encoded = encode_ftv1_trace(&*trace_result);
                    new_attrs.insert(KeyValue::new(APOLLO_PRIVATE_FTV1, encoded));
                }
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
            attributes: span.attributes.clone(),
            events: EvictedQueue::new(0),
            links: EvictedQueue::new(0),
            status,
            resource: Cow::Owned(self.resource_template.to_owned()),
            instrumentation_lib: self.intrumentation_library.clone(),
        }
    }
    

    pub(crate) fn export(&self, spans: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let mut exporter = self.otlp_exporter.lock();
        exporter.export(spans)
    }

    pub(crate) fn shutdown(&self) {
        let mut exporter = self.otlp_exporter.lock();
        exporter.shutdown()
    }
}