//! Trace Ids for the router.

#![warn(unreachable_pub)]
use std::fmt;

use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId as OtelTraceId;
use serde::Serialize;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Trace ID
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Create a TraceId. If called from an invalid context
    /// (e.g.: not in a span, or in a disabled span), then
    /// None is returned.
    pub fn maybe_new() -> Option<Self> {
        let trace_id = Span::current().context().span().span_context().trace_id();
        if trace_id == OtelTraceId::INVALID {
            None
        } else {
            Some(Self(trace_id.to_bytes()))
        }
    }

    /// Convert the TraceId to bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Convert the TraceId to u128.
    pub fn to_u128(&self) -> u128 {
        u128::from_be_bytes(self.0)
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.to_u128())
    }
}

#[cfg(test)]
mod test {
    use std::sync::Mutex;

    use once_cell::sync::Lazy;
    use opentelemetry::sdk::export::trace::stdout;
    use tracing::span;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    use super::TraceId;

    // If we try to run more than one test concurrently which relies on the existence of a pipeline,
    // then the tests will fail due to manipulation of global state in the opentelemetry crates.
    // If we set test-threads=1, then this avoids the problem but means all our tests will run slowly.
    // So: to avoid this problem, we have a mutex lock which just exists to serialize access to the
    // global resources.
    static TRACING_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[test]
    fn it_returns_invalid_trace_id() {
        let my_id = TraceId::maybe_new();
        assert!(my_id.is_none());
    }

    #[test]
    fn it_correctly_compares_invalid_and_invalid_trace_id() {
        let my_id = TraceId::maybe_new();
        let other_id = TraceId::maybe_new();
        assert!(my_id.is_none());
        assert!(other_id.is_none());
        assert!(other_id == my_id);
    }

    #[test]
    fn it_returns_valid_trace_id() {
        let _guard = TRACING_LOCK.lock().unwrap();
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
            assert!(TraceId::maybe_new().is_some());
        });
    }

    #[test]
    fn it_correctly_compares_valid_and_invalid_trace_id() {
        let _guard = TRACING_LOCK.lock().unwrap();
        let my_id = TraceId::maybe_new();
        assert!(my_id.is_none());
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

            let other_id = TraceId::maybe_new();
            assert!(other_id.is_some());
            assert_ne!(other_id, my_id);
        });
    }

    #[test]
    fn it_correctly_compares_valid_and_valid_trace_id() {
        let _guard = TRACING_LOCK.lock().unwrap();
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

            let my_id = TraceId::maybe_new();
            assert!(my_id.is_some());
            let other_id = TraceId::maybe_new();
            assert_eq!(other_id, my_id);
        });
    }
}
