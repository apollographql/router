use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use derivative::Derivative;
use futures::future::BoxFuture;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::sdk::trace::EvictedQueue;
use opentelemetry::sdk::Resource;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Status;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::InstrumentationLibrary;
use opentelemetry::KeyValue;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::EvictedHashMap;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
use parking_lot::Mutex;
use sys_info::hostname;
use tonic::metadata::MetadataMap;
use tonic::metadata::MetadataValue;
use tower::BoxError;
use url::Url;
use uuid::Uuid;

use super::apollo::ErrorsConfiguration;
use super::apollo::OperationSubType;
use super::config_new::attributes::SUBGRAPH_NAME;
use super::otel::PreSampledTracer;
use super::otlp::Protocol;
use super::reload::OPENTELEMETRY_TRACER_HANDLE;
use super::tracing::apollo_telemetry::encode_ftv1_trace;
use super::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
use super::tracing::apollo_telemetry::extract_i64;
use super::tracing::apollo_telemetry::extract_string;
use super::tracing::apollo_telemetry::LightSpanData;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS_KEY;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_FTV1;
use super::tracing::apollo_telemetry::APOLLO_PRIVATE_REQUEST;
use super::tracing::apollo_telemetry::OPERATION_SUBTYPE;
use crate::plugins::telemetry::apollo::ROUTER_ID;
use crate::plugins::telemetry::apollo_exporter::get_uname;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::services::OperationKind;

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
    include_span_names: HashSet<&'static str>,
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
        include_span_names: HashSet<&'static str>,
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
            errors_configuration: errors_configuration.clone(),
            include_span_names,
        });
    }

    pub(crate) fn prepare_for_export(&self, trace_spans: Vec<LightSpanData>) -> Vec<SpanData> {
        let mut export_spans: Vec<SpanData> = Vec::new();

        trace_spans.into_iter().for_each(|span| {
            if span.attributes.get(&APOLLO_PRIVATE_REQUEST).is_some()
                || self.include_span_names.contains(span.name.as_ref())
            {
                match span.name.as_ref() {
                    SUBGRAPH_SPAN_NAME => export_spans.push(self.prepare_subgraph_span(span)),
                    EXECUTION_SPAN_NAME => export_spans.push(self.prepare_execution_span(span)),
                    SUBSCRIPTION_EVENT_SPAN_NAME => {
                        if let Some(request_span) =
                            self.synthesize_request_span_for_subscription_event(&span)
                        {
                            let child_span =
                                self.prepare_subscription_event_span(span, &request_span);
                            export_spans.push(request_span);
                            export_spans.push(child_span);
                        }
                    }
                    _ => export_spans.push(self.base_prepare_span(span)),
                };
            }
        });
        export_spans
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
            // TBD(tim): if the underlying exporter supported it, we could
            // group by resource attributes here and significantly reduce the
            // duplicate resource / scope data that will get sent on every span.
            resource: Cow::Owned(self.resource_template.to_owned()),
            instrumentation_lib: self.intrumentation_library.clone(),
        }
    }

    /// Adds the "graphql.operation.subtype" attribute for subscription requests.
    /// TBD(tim): we could do this for all OTLP?
    /// or not do this at all and let the backend interpret?
    fn prepare_execution_span(&self, mut span: LightSpanData) -> SpanData {
        let op_type = span
            .attributes
            .get(&GRAPHQL_OPERATION_TYPE)
            .and_then(extract_string)
            .unwrap_or_default();
        if op_type == OperationKind::Subscription.as_apollo_operation_type() {
            // Currently, all "subscription" operations are of the "request" variety.
            span.attributes.insert(KeyValue::new(
                OPERATION_SUBTYPE,
                OperationSubType::SubscriptionRequest.as_str(),
            ));
        }
        self.base_prepare_span(span)
    }

    fn synthesize_request_span_for_subscription_event(
        &self,
        sub_event_span: &LightSpanData,
    ) -> Option<SpanData> {
        let tracer = OPENTELEMETRY_TRACER_HANDLE
            .get()
            .expect("expected a tracer");
        let span_id = tracer.new_span_id();
        let span_name = format!(
            "{} {}",
            OperationKind::Subscription.as_apollo_operation_type(),
            sub_event_span
                .attributes
                .get(&GRAPHQL_OPERATION_NAME)
                .and_then(extract_string)
                .unwrap_or_default()
        );
        let mut request_span = SpanData {
            span_context: SpanContext::new(
                sub_event_span.trace_id,
                span_id,
                TraceFlags::default().with_sampled(true),
                true,
                TraceState::default(),
            ),
            parent_span_id: SpanId::from(0u64),
            span_kind: SpanKind::Server,
            name: span_name.into(),
            start_time: sub_event_span.start_time,
            end_time: sub_event_span.end_time,
            attributes: EvictedHashMap::new(10, 10),
            events: EvictedQueue::new(0),
            links: EvictedQueue::new(0),
            status: sub_event_span.status.clone(),
            resource: Cow::Owned(self.resource_template.to_owned()),
            instrumentation_lib: self.intrumentation_library.clone(),
        };
        request_span
            .attributes
            .insert(KeyValue::new(APOLLO_PRIVATE_REQUEST, true));
        Some(request_span)
    }

    /// Sets the parent span ID
    fn prepare_subscription_event_span(
        &self,
        mut span: LightSpanData,
        request_span: &SpanData,
    ) -> SpanData {
        if let Some(duration_ns) = span
            .attributes
            .get(&APOLLO_PRIVATE_DURATION_NS_KEY)
            .and_then(extract_i64)
            .map(|f| f as u64)
        {
            span.end_time = span.start_time
                + Duration::new(
                    duration_ns / 1_000_000_000,
                    (duration_ns % 1_000_000_000)
                        .try_into()
                        .expect("math should work"),
                );
        }
        span.parent_span_id = request_span.span_context.span_id();
        span.attributes.insert(KeyValue::new(
            OPERATION_SUBTYPE,
            OperationSubType::SubscriptionEvent.as_str(),
        ));
        self.base_prepare_span(span)
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
        exporter.export(spans)
    }

    pub(crate) fn shutdown(&self) {
        let mut exporter = self.otlp_exporter.lock();
        exporter.shutdown()
    }
}
