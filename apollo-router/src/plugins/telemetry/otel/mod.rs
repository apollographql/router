/// Implementation of the trace::Layer as a source of OpenTelemetry data.
pub(crate) mod layer;
pub(crate) mod named_runtime_channel;
/// Span extension which enables OpenTelemetry context management.
pub(crate) mod span_ext;
/// Protocols for OpenTelemetry Tracers that are compatible with Tracing
pub(crate) mod tracer;

pub(crate) use layer::OpenTelemetryLayer;
pub(crate) use layer::layer;
use opentelemetry::Key;
use opentelemetry::Value;
pub(crate) use span_ext::OpenTelemetrySpanExt;
pub(crate) use tracer::PreSampledTracer;

/// Per-span OpenTelemetry data tracked by this crate.
///
/// Useful for implementing [PreSampledTracer] in alternate otel SDKs.
#[derive(Debug, Clone, Default)]
pub(crate) struct OtelData {
    /// The parent otel `Context` for the current tracing span.
    pub(crate) parent_cx: opentelemetry::Context,

    /// The otel span data recorded during the current tracing span.
    pub(crate) builder: opentelemetry::trace::SpanBuilder,

    /// Attributes gathered for the next event
    #[cfg(not(test))]
    pub(crate) event_attributes: Option<ahash::HashMap<Key, Value>>,
    #[cfg(test)]
    pub(crate) event_attributes: Option<indexmap::IndexMap<Key, Value>>,

    /// Forced status in case it's coming from the custom attributes
    pub(crate) forced_status: Option<opentelemetry::trace::Status>,

    /// Forced span name in case it's coming from the custom attributes
    pub(crate) forced_span_name: Option<String>,
}
