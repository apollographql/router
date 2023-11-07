use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use opentelemetry::trace::TraceId;
use opentelemetry::Key;
use opentelemetry_api::trace::TraceContextExt;
use std::collections::HashMap;
use tower::BoxError;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// These modules contain a new config structure for telemetry that will progressively move to
pub(crate) mod attributes;
pub(crate) mod conditions;

pub(crate) mod events;
pub(crate) mod extendable;
pub(crate) mod instruments;
pub(crate) mod logging;
pub(crate) mod selectors;
pub(crate) mod spans;

pub(crate) trait GetAttributes<Request, Response> {
    fn on_request(&self, request: &Request) -> HashMap<Key, AttributeValue>;
    fn on_response(&self, response: &Response) -> HashMap<Key, AttributeValue>;
    fn on_error(&self, error: &BoxError) -> HashMap<Key, AttributeValue>;
}

pub(crate) trait GetAttribute<Request, Response> {
    fn on_request(&self, request: &Request) -> Option<AttributeValue>;
    fn on_response(&self, response: &Response) -> Option<AttributeValue>;
}

pub(crate) trait DefaultForLevel {
    fn defaults_for_level(&mut self, requirement_level: &DefaultAttributeRequirementLevel);
}

pub(crate) trait DatadogId {
    fn to_datadog(&self) -> String;
}
impl DatadogId for TraceId {
    fn to_datadog(&self) -> String {
        let bytes = &self.to_bytes()[std::mem::size_of::<u64>()..std::mem::size_of::<u128>()];
        u64::from_be_bytes(bytes.try_into().unwrap()).to_string()
    }
}

pub(crate) fn trace_id() -> Option<TraceId> {
    let context = Span::current().context();
    let span = context.span();
    let span_context = span.span_context();
    if span_context.is_valid() {
        Some(span_context.trace_id())
    } else {
        None
    }
}

use opentelemetry::baggage::BaggageExt;
pub(crate) fn get_baggage(key: &str) -> Option<AttributeValue> {
    let context = Span::current().context();
    let baggage = context.baggage();
    baggage
        .get(key.to_string())
        .map(|v| AttributeValue::from(v.clone()))
}

#[cfg(test)]
mod test {
    use crate::plugins::telemetry::config_new::{trace_id, DatadogId};
    use opentelemetry_api::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry_api::Context;
    use tracing::span;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn dd_convert() {
        let trace_id = TraceId::from_hex("234e10d9e749a0a19e94ac0e4a896aee").unwrap();
        let dd_id = trace_id.to_datadog();
        assert_eq!(dd_id, "11426947331925830382");
    }

    #[test]
    fn test_trace_id() {
        // Create a span with a trace ID
        let subscriber = tracing_subscriber::registry().with(tracing_opentelemetry::layer());
        tracing::subscriber::with_default(subscriber, || {
            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                TraceFlags::default(),
                false,
                TraceState::default(),
            );
            let _context = Context::current()
                .with_remote_span_context(span_context)
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();
            let trace_id = trace_id();
            assert_eq!(trace_id, Some(TraceId::from_u128(42)));
        });
    }

    #[test]
    fn test_baggage() {
        // Create a span with a trace ID
        let subscriber = tracing_subscriber::registry().with(tracing_opentelemetry::layer());
        tracing::subscriber::with_default(subscriber, || {
            let span_context = SpanContext::new(
                TraceId::from_u128(42),
                SpanId::from_u64(42),
                TraceFlags::default(),
                false,
                TraceState::default(),
            );
            let _context = Context::current()
                .with_remote_span_context(span_context)
                .attach();
            let span = span!(tracing::Level::INFO, "test");
            let _guard = span.enter();
            let trace_id = trace_id();
            assert_eq!(trace_id, Some(TraceId::from_u128(42)));
        });
    }
}
