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

#[cfg(test)]
mod test {
    use crate::plugins::telemetry::config_new::DatadogId;
    use opentelemetry_api::baggage::BaggageExt;
    use opentelemetry_api::trace::TraceId;

    #[test]
    fn dd_convert() {
        let trace_id = TraceId::from_hex("234e10d9e749a0a19e94ac0e4a896aee").unwrap();
        let dd_id = trace_id.to_datadog();
        assert_eq!(dd_id, "11426947331925830382");
    }
}
