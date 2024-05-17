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

use super::tracing::apollo_telemetry::LightSpanData;
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
        batch_config: &BatchProcessorConfig,
        apollo_key: &str,
        apollo_graph_ref: &str,
        schema_id: &str,
    ) -> Result<ApolloOtlpExporter, BoxError> {
        tracing::debug!(endpoint = %endpoint, "creating Apollo OTLP traces exporter");

        let mut metadata = MetadataMap::new();
        metadata.insert("apollo.api.key", MetadataValue::try_from(apollo_key)?);

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
            otlp_exporter: Arc::new(Mutex::new(
                SpanExporterBuilder::from(
                    opentelemetry_otlp::new_exporter()
                        .tonic()
                        .with_timeout(batch_config.max_export_timeout)
                        .with_endpoint(endpoint.to_string())
                        .with_metadata(metadata),
                        // TBD(tim): figure out why compression seems to be turned off on our collector
                        // .with_compression(opentelemetry_otlp::Compression::Gzip),
                )
                .build_span_exporter()?,
            )),
            // TBD(tim): do we need another batch processor for this?
            // Seems like we've already set up a batcher earlier in the pipe but not quite sure.
        });
    }

    pub(crate) fn prepare_for_export(&self, span: &LightSpanData) -> SpanData {
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
            status: Status::Unset,
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
