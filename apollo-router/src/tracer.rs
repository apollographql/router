//! Trace Ids for the router.

#![warn(unreachable_pub)]
#![warn(missing_docs)]

use opentelemetry::trace::TraceContextExt;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Trace ID
#[derive(Debug, PartialEq, Eq)]
pub struct TraceId([u8; 16]);

/// Invalid Trace ID
pub const TRACE_INVALID: TraceId = TraceId([0; 16]);

/// Return a trace_id. If called from an invalid context
/// (e.g.: not in a span, or in a disabled span), then
/// the value of the TraceId is [`TRACE_INVALID`].
pub fn trace_id() -> TraceId {
    TraceId(
        Span::current()
            .context()
            .span()
            .span_context()
            .trace_id()
            .to_bytes(),
    )
}

impl TraceId {
    /// Convert the TraceId to bytes.
    pub fn to_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[cfg(test)]
mod test {
    use super::trace_id;
    use super::TRACE_INVALID;
    use opentelemetry::sdk::export::trace::stdout;
    use tracing::span;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    #[test]
    fn it_returns_invalid_trace_id() {
        let my_id = trace_id();
        assert_eq!(my_id, TRACE_INVALID);
        assert_eq!(my_id.to_bytes(), [0; 16])
    }

    #[test]
    fn it_returns_valid_trace_id() {
        // Create a new OpenTelemetry pipeline
        let tracer = stdout::new_pipeline().install_simple();
        // Create a tracing layer with the configured tracer
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);
        // Trace executed code
        tracing::subscriber::with_default(subscriber, || {
            // Spans will be sent to the configured OpenTelemetry exporter
            let root = span!(tracing::Level::TRACE, "trace test");
            let _enter = root.enter();

            assert!(trace_id() != TRACE_INVALID);
        });
    }
}
