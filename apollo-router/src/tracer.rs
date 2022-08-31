//! Trace Ids for the router.

#![warn(unreachable_pub)]
#![warn(missing_docs)]
use std::fmt;

use opentelemetry::trace::TraceContextExt;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Trace ID
#[derive(Debug, PartialEq, Eq)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Invalid Trace ID
    pub const INVALID: TraceId = TraceId([0; 16]);

    /// Create a TraceId. If called from an invalid context
    /// (e.g.: not in a span, or in a disabled span), then
    /// the value of the TraceId is [`TraceId::INVALID`].
    pub fn new() -> Self {
        TraceId(
            Span::current()
                .context()
                .span()
                .span_context()
                .trace_id()
                .to_bytes(),
        )
    }

    /// Convert the TraceId to bytes.
    pub fn to_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::sdk::export::trace::stdout;
    use tracing::span;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    use super::TraceId;

    #[test]
    fn it_returns_invalid_trace_id() {
        let my_id = TraceId::new();
        assert_eq!(my_id, TraceId::INVALID);
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

            assert!(TraceId::new() != TraceId::INVALID);
        });
    }
}
