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
/// (`OpenTelemetryLayer`) builds the real span and resolves `current_cx` before the
/// `OtelData` is even constructed - there's no "not yet built" state to represent,
/// because nothing outside `on_new_span`'s own local variables ever sees this type
/// before it's fully built.
#[derive(Debug, Clone, Default)]
pub(crate) struct OtelData {
    /// The live otel `Span`, wrapped in its `Context`. Always already built - see the
    /// struct-level doc comment.
    pub(crate) current_cx: opentelemetry::Context,

    /// Mirrors every attribute set on this span. A live `Span` (via `SpanRef`) offers
    /// no way to read attributes back once set, so this is kept in sync for code that
    /// needs to inspect a span's attributes (log formatting, log correlation, response
    /// headers, etc).
    pub(crate) attributes: Vec<KeyValue>,

    /// The tracing span's original name, from before any `forced_span_name`
    /// override. A live `Span` offers no way to read its current name back once
    /// set, so this is captured up front and used to record `OTEL_ORIGINAL_NAME`
    /// on close.
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
    /// Adds `kv` to the span, replacing any existing attribute with the same key in
    /// `attributes` (this crate's own mirror, kept in sync so it can be read back
    /// later regardless of what the live span itself allows).
    ///
    /// The live span has no replace-by-key primitive at all:
    /// `opentelemetry_sdk::trace::Span::set_attribute` is an unconditional push into
    /// storage this crate can't read back or rewrite. Setting the same key twice
    /// therefore still exports both values there; this is expected to be resolved as
    /// last-value-wins by the consuming backend, which is standard OTel practice for
    /// duplicate-key attributes (and matches the spec's stated "overwrite" intent, even
    /// though this particular SDK doesn't enforce it internally).
    pub(crate) fn upsert_attribute(&mut self, kv: KeyValue) {
        upsert_attribute(&mut self.attributes, kv.clone());
        self.current_cx.span().set_attribute(kv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_attribute_replaces_same_key() {
        let mut otel_data = OtelData::default();

        otel_data.upsert_attribute(KeyValue::new("cache.status", "MISS"));
        otel_data.upsert_attribute(KeyValue::new("cache.status", "HIT"));

        assert_eq!(
            otel_data.attributes,
            vec![KeyValue::new("cache.status", "HIT")]
        );
    }
}
