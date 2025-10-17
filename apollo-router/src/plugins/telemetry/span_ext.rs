use opentelemetry::Key;
use opentelemetry::KeyValue;
use opentelemetry::Value;

use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_DESCRIPTION;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;

/// To add dynamic attributes for spans
pub(crate) trait SpanMarkError {
    fn mark_as_error(&self, error_message: String);
}

impl SpanMarkError for ::tracing::Span {
    fn mark_as_error(&self, error_message: String) {
        self.set_span_dyn_attributes([
            KeyValue::new(
                Key::from_static_str(OTEL_STATUS_CODE),
                Value::String("ERROR".to_string().into()),
            ),
            KeyValue::new(
                Key::from_static_str(OTEL_STATUS_DESCRIPTION),
                Value::String(error_message.into()),
            ),
        ]);
    }
}
