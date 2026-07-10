/// Implementation of the trace::Layer as a source of OpenTelemetry data.
pub(crate) mod layer;
/// Span extension which enables OpenTelemetry context management.
pub(crate) mod span_ext;

pub(crate) use layer::OpenTelemetryLayer;
pub(crate) use layer::layer;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::Value;
use opentelemetry::trace::TraceContextExt;
pub(crate) use span_ext::OpenTelemetrySpanExt;

use super::utils::upsert_attribute;

/// Per-span OpenTelemetry data tracked by this crate.
///
/// As of opentelemetry 0.32, `SpanBuilder` no longer carries `trace_id`, `span_id`,
/// `status`, `end_time` or `sampling_result` (they were removed upstream). Upstream
/// `tracing-opentelemetry` responds to that by building the real otel `Span` lazily -
/// only the first time something needs a context for it. This crate does *not* take
/// that approach: router code (log correlation, response headers) expects a sampled
/// span's trace/span id to be available as soon as it's created, so `on_new_span`
/// promotes [`OtelDataState::Builder`] to a real, live [`OtelDataState::Context`]
/// immediately, before it returns - see `OpenTelemetryLayer::start_cx`. `Builder`
/// only exists as a transient buffer for attributes gathered before that promotion,
/// and as the state the `TestTracer` test harness parks in indefinitely so unit
/// tests can inspect builder fields without a real tracer.
#[derive(Debug, Clone, Default)]
pub(crate) struct OtelData {
    /// The state of the span: still buffering into a builder, or already built into
    /// a live otel `Span` wrapped in a `Context`. Always `Context` by the time any
    /// callback other than `on_new_span` observes it, outside of tests.
    pub(crate) state: OtelDataState,

    /// Mirrors every attribute set on this span. A live `Span` (via `SpanRef`) offers
    /// no way to read attributes back once set, so this is kept in sync with the
    /// builder/live span regardless of `state`, for code that needs to inspect a
    /// span's attributes (log formatting, log correlation, response headers, etc).
    pub(crate) attributes: Vec<KeyValue>,

    /// The tracing span's original name, from before any `forced_span_name`
    /// override. A live `Span` offers no way to read its current name back once
    /// set, so this is captured up front and used to record `OTEL_ORIGINAL_NAME`
    /// on close regardless of whether the span was already built at that point.
    pub(crate) original_name: &'static str,

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

impl OtelData {
    /// Adds or replaces `kv` on the span, whether it's still buffering into a builder
    /// or has already been built for real - and keeps `attributes` in sync so it can
    /// be read back later regardless.
    pub(crate) fn push_attribute(&mut self, kv: KeyValue) {
        upsert_attribute(&mut self.attributes, kv.clone());
        match &mut self.state {
            OtelDataState::Builder { builder, .. } => {
                builder.attributes.get_or_insert_with(Vec::new).push(kv);
            }
            OtelDataState::Context { current_cx } => current_cx.span().set_attribute(kv),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum OtelDataState {
    /// The span hasn't been built yet: data is buffered into `builder` and applied
    /// once something forces a transition to `Context`.
    Builder {
        parent_cx: opentelemetry::Context,
        builder: opentelemetry::trace::SpanBuilder,
        status: opentelemetry::trace::Status,
    },
    /// The span has been built for real and is live; `current_cx` wraps it.
    Context { current_cx: opentelemetry::Context },
}

impl Default for OtelDataState {
    fn default() -> Self {
        OtelDataState::Context {
            current_cx: opentelemetry::Context::default(),
        }
    }
}
