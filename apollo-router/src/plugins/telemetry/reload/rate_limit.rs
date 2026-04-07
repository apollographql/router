//! Rate limiting layer for log messages.
//!
//! Some libraries can emit frequent error/warning messages when things go wrong (e.g., export
//! failures, shutdown issues). This module provides a tracing layer that rate-limits these
//! messages to avoid log spam while still surfacing important errors.
//!
//! The rate limiting works by tracking each unique callsite (log location) and only allowing
//! one message per callsite per time window (10 seconds by default).

use std::time::Duration;
use std::time::Instant;

use dashmap::DashMap;
use tracing::Subscriber;
use tracing::callsite::Identifier;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// State for tracking rate limiting of a single callsite.
#[derive(Default)]
struct RateLimitState {
    /// Last time a message from this callsite was logged. None means never logged.
    last_logged: Option<Instant>,
    /// Count of suppressed messages since last log.
    suppressed_count: u64,
}

/// A tracing layer that rate-limits log messages from a specific target prefix.
///
/// This layer intercepts events from targets starting with a configurable prefix and applies
/// rate limiting based on callsite. Messages from each unique callsite are allowed through
/// at most once per time window, with suppressed message counts reported periodically.
pub(crate) struct RateLimitLayer {
    /// Target prefix to match (e.g., "opentelemetry").
    target_prefix: &'static str,
    /// Rate limit states keyed by callsite identifier.
    states: DashMap<Identifier, RateLimitState>,
    /// Time window for rate limiting.
    threshold: Duration,
}

impl RateLimitLayer {
    /// Create a new rate limiting layer for the specified target prefix and time window.
    pub(crate) fn new(target_prefix: &'static str, threshold: Duration) -> Self {
        Self {
            target_prefix,
            states: DashMap::new(),
            threshold,
        }
    }

    /// Returns the total number of suppressed messages across all callsites.
    #[cfg(test)]
    fn suppressed_count(&self) -> u64 {
        self.states.iter().map(|e| e.suppressed_count).sum()
    }

    /// Check if a message should be allowed through.
    ///
    /// Returns `true` if the message should be logged, `false` if rate limited.
    fn is_allowed(&self, callsite: Identifier) -> bool {
        let now = Instant::now();

        let mut entry = self.states.entry(callsite.clone()).or_default();
        let state = entry.value_mut();

        let allowed = match state.last_logged {
            None => true, // First message from this callsite
            Some(last) => now.duration_since(last) >= self.threshold,
        };

        if allowed {
            state.last_logged = Some(now);
            state.suppressed_count = 0;
        } else {
            state.suppressed_count += 1;
        }

        allowed
    }
}

impl<S> Layer<S> for RateLimitLayer
where
    S: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn event_enabled(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) -> bool {
        let metadata = event.metadata();

        // Only apply rate limiting to matching targets
        if !metadata.target().starts_with(self.target_prefix) {
            return true;
        }

        // Only rate limit WARN and ERROR level messages
        // DEBUG/INFO/TRACE are already filtered by log level
        if *metadata.level() > tracing::Level::WARN {
            return true;
        }

        self.is_allowed(metadata.callsite())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use tracing::Subscriber;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Test layer that counts events.
    struct CountingLayer {
        count: Arc<AtomicUsize>,
    }

    impl<S: Subscriber> Layer<S> for CountingLayer {
        fn on_event(&self, _event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_rate_limiting_suppresses_rapid_messages() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // Log multiple messages rapidly
            for _ in 0..10 {
                tracing::warn!(target: "opentelemetry::trace::exporter", "export failed");
            }
        });

        // Only one message should have gotten through
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_different_callsites_not_suppressed() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // Each line is a different callsite, so each should get through
            tracing::warn!(target: "opentelemetry::trace::exporter", "trace error");
            tracing::warn!(target: "opentelemetry::metrics::exporter", "metric error");
            tracing::warn!(target: "opentelemetry::other", "other error");
        });

        // Each callsite should log once
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_non_otel_messages_not_affected() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // Log non-otel messages
            for _ in 0..10 {
                tracing::warn!(target: "apollo_router", "normal message");
            }
        });

        // All messages should get through
        assert_eq!(count.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn test_messages_allowed_after_threshold() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(50));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        // Helper to emit from same callsite
        fn emit_otel_warning() {
            tracing::warn!(target: "opentelemetry::trace", "message");
        }

        tracing::subscriber::with_default(subscriber, || {
            emit_otel_warning(); // First - allowed
            emit_otel_warning(); // Second - suppressed (same callsite, within threshold)

            // Wait for threshold
            std::thread::sleep(Duration::from_millis(60));

            emit_otel_warning(); // Third - allowed (threshold elapsed)
        });

        // First and third messages should get through
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_debug_level_not_rate_limited() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // Debug level messages should not be rate limited
            for _ in 0..5 {
                tracing::debug!(target: "opentelemetry::trace", "debug message");
            }
        });

        // All debug messages should get through (rate limiting only applies to WARN+)
        assert_eq!(count.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_warn_level_is_rate_limited() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // WARN level should be rate limited
            for _ in 0..10 {
                tracing::warn!(target: "opentelemetry::trace::exporter", "export warning");
            }
        });

        // Only one message should have gotten through
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_error_level_is_rate_limited() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // ERROR level should also be rate limited
            for _ in 0..10 {
                tracing::error!(target: "opentelemetry::trace::exporter", "export error");
            }
        });

        // Only one message should have gotten through
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_info_level_not_rate_limited() {
        let count = Arc::new(AtomicUsize::new(0));
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        let subscriber = tracing_subscriber::registry()
            .with(rate_limiter)
            .with(CountingLayer {
                count: count.clone(),
            });

        tracing::subscriber::with_default(subscriber, || {
            // INFO level messages should not be rate limited (only WARN and ERROR are)
            for _ in 0..5 {
                tracing::info!(target: "opentelemetry::trace", "info message");
            }
        });

        // All INFO messages should get through
        assert_eq!(count.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_suppression_count_tracked() {
        // Test is_allowed directly to verify counting
        let rate_limiter = RateLimitLayer::new("opentelemetry", Duration::from_millis(100));

        // Create a fake callsite identifier for testing
        static TEST_CALLSITE: tracing_core::callsite::DefaultCallsite =
            tracing_core::callsite::DefaultCallsite::new(&TEST_META);
        static TEST_META: tracing_core::Metadata<'static> = tracing_core::metadata! {
            name: "test",
            target: "opentelemetry::test",
            level: tracing_core::Level::WARN,
            fields: &[],
            callsite: &TEST_CALLSITE,
            kind: tracing_core::metadata::Kind::EVENT,
        };

        let callsite = TEST_META.callsite();

        // First call should be allowed
        assert!(rate_limiter.is_allowed(callsite.clone()));
        assert_eq!(rate_limiter.suppressed_count(), 0);

        // Subsequent calls within threshold should be suppressed
        for i in 1..=9 {
            assert!(!rate_limiter.is_allowed(callsite.clone()));
            assert_eq!(rate_limiter.suppressed_count(), i);
        }
    }
}
