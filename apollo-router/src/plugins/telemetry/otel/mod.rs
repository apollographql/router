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
/// `current_cx` always holds a live, already-built otel span — `OtelData` is never
/// constructed before the span is built, so there is no "building" state to represent.
#[derive(Debug, Clone, Default)]
pub(crate) struct OtelData {
    /// The live otel `Span`, wrapped in its `Context`. Always already built.
    pub(crate) current_cx: opentelemetry::Context,

    /// Mirrors every attribute set on this span. A live span's attributes aren't
    /// readable once set; this vec keeps them accessible.
    pub(crate) attributes: Vec<KeyValue>,

    /// The span's original name, captured before any `forced_span_name` override.
    /// A live span's name isn't readable once set.
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
    /// Adds `kv` to the span, replacing any existing value for the same key in
    /// `attributes`. The live span receives an additional `set_attribute` call
    /// regardless; backends typically resolve duplicate-key attributes as
    /// last-value-wins.
    pub(crate) fn upsert_attribute(&mut self, kv: KeyValue) {
        upsert_attribute(&mut self.attributes, kv.clone());
        // `Span::set_attribute` is an unconditional push with no replace-by-key
        // primitive, so setting the same key twice results in both values being
        // exported. Backends are expected to resolve this as last-value-wins
        // (standard OTel practice), so the duplicate is harmless in practice.
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
