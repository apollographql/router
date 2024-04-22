//! Trace Ids for the router.

#![warn(unreachable_pub)]
use std::fmt;

use opentelemetry::trace::TraceContextExt;
use serde::Deserialize;
use serde::Serialize;
use tracing::Span;

use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;

/// Trace ID
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Create a TraceId. If the span is not sampled then return None.
    pub fn maybe_new() -> Option<Self> {
        let span = Span::current();
        let context = span.context();
        let span_ref = context.span();
        let span_context = span_ref.span_context();
        if span_context.is_sampled() {
            Some(Self(span_context.trace_id().to_bytes()))
        } else {
            None
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

// Note: These tests all end up writing what look like dbg!() spans to stdout when the tests are
// run as part of the full suite.
// Why? It's probably related to the way that the rust test framework tries to capture test
// output. I spent a little time investigating it and concluded it will be harder to fix than to
// live with...
#[cfg(test)]
mod test {
    use std::sync::Mutex;

    use once_cell::sync::Lazy;
    use opentelemetry::trace::TracerProvider;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    use super::TraceId;
    use crate::plugins::telemetry::otel;

    // If we try to run more than one test concurrently which relies on the existence of a pipeline,
    // then the tests will fail due to manipulation of global state in the opentelemetry crates.
    // If we set test-threads=1, then this avoids the problem but means all our tests will run slowly.
    // So: to avoid this problem, we have a mutex lock which just exists to serialize access to the
    // global resources.
    // Note: If a test fails, then it will poison the lock, so when locking we attempt to recover
    // from poisoned mutex and continue anyway. This is safe to do, since the lock is effectively
    // "read-only" and not protecting shared state but synchronising code access to global state.
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

    #[tokio::test]
    async fn it_returns_valid_trace_id() {
        let _guard = TRACING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Create a tracing layer with the configured tracer

        let provider = opentelemetry::sdk::trace::TracerProvider::builder()
            .with_simple_exporter(
                opentelemetry_stdout::SpanExporter::builder()
                    .with_writer(std::io::stdout())
                    .build(),
            )
            .build();
        let tracer = provider.versioned_tracer("noop", None::<String>, None::<String>, None);

        let telemetry = otel::layer().with_tracer(tracer);
        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);
        // Trace executed code
        tracing::subscriber::with_default(subscriber, || {
            // Spans will be sent to the configured OpenTelemetry exporter
            let _span = tracing::trace_span!("trace test").entered();
            assert!(TraceId::maybe_new().is_some());
        });
    }

    #[test]
    fn it_correctly_compares_valid_and_invalid_trace_id() {
        let _guard = TRACING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let my_id = TraceId::maybe_new();
        assert!(my_id.is_none());
        // Create a tracing layer with the configured tracer
        let provider = opentelemetry::sdk::trace::TracerProvider::builder()
            .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
            .build();
        let tracer = provider.versioned_tracer("noop", None::<String>, None::<String>, None);
        let telemetry = otel::layer().with_tracer(tracer);
        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);
        // Trace executed code
        tracing::subscriber::with_default(subscriber, || {
            // Spans will be sent to the configured OpenTelemetry exporter
            let _span = tracing::trace_span!("trace test").entered();

            let other_id = TraceId::maybe_new();
            assert!(other_id.is_some());
            assert_ne!(other_id, my_id);
        });
    }

    #[test]
    fn it_correctly_compares_valid_and_valid_trace_id() {
        let _guard = TRACING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Create a tracing layer with the configured tracer
        let provider = opentelemetry::sdk::trace::TracerProvider::builder()
            .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
            .build();
        let tracer = provider.versioned_tracer("noop", None::<String>, None::<String>, None);
        let telemetry = otel::layer().with_tracer(tracer);
        // Use the tracing subscriber `Registry`, or any other subscriber
        // that impls `LookupSpan`
        let subscriber = Registry::default().with(telemetry);
        // Trace executed code
        tracing::subscriber::with_default(subscriber, || {
            // Spans will be sent to the configured OpenTelemetry exporter
            let _span = tracing::trace_span!("trace test").entered();

            let my_id = TraceId::maybe_new();
            assert!(my_id.is_some());
            let other_id = TraceId::maybe_new();
            assert_eq!(other_id, my_id);
        });
    }
}
