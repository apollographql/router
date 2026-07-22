//! Volume-reduction strategies for traces exported to Apollo Studio.
//!
//! These only affect the Apollo trace pipeline (see [`super::apollo`]); the customer OTLP and
//! Datadog pipelines and the global head sampler are untouched. Both strategies make a keep/drop
//! decision on a *complete, reassembled* trace, so they run inside
//! [`super::apollo_telemetry::Exporter::export`] rather than as a span processor (a per-span
//! `on_end` hook cannot see the operation/client/latency of the whole trace).

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::num::NonZeroUsize;
use std::time::Duration;
use std::time::Instant;
use std::time::UNIX_EPOCH;

use lru::LruCache;
use opentelemetry::Key;
use opentelemetry::Value;
use opentelemetry::trace::Status;
use parking_lot::Mutex;

use crate::plugins::telemetry::apollo::ApolloTraceThrottleConfig;
use crate::plugins::telemetry::metrics::apollo::histogram::duration_bucket;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_NAME_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_VERSION_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::GRAPHQL_ERROR_EXT_CODE;
use crate::plugins::telemetry::tracing::apollo_telemetry::LightSpanData;
use crate::plugins::telemetry::tracing::apollo_telemetry::OPERATION_SUBTYPE;
use crate::plugins::telemetry::tracing::apollo_telemetry::OPERATION_TYPE;

/// Maximum number of distinct dimension combinations remembered by the representative-traces
/// filter. Because the trace's end-minute is part of every key, entries for elapsed minutes can
/// never match again and are evicted under LRU pressure, so this only needs to be large enough to
/// hold one minute's worth of distinct combinations for a busy graph.
const REPRESENTATIVE_CACHE_CAPACITY: usize = 100_000;

/// Fixed per-instance cap on traces exported per second in `rate_limited` mode. Deliberately not
/// user-configurable (see INS-1975).
const MAX_TRACES_PER_SECOND: u32 = 100;

/// Runtime trace throttle for the Apollo export pipeline, built from [`ApolloTraceThrottleConfig`].
///
/// Uses interior mutability so the decision can be made from `&self` in the exporter's `export`.
#[derive(Debug)]
pub(crate) enum ApolloTraceThrottle {
    RepresentativeTraces(RepresentativeTraceFilter),
    RateLimited(TraceRateLimiter),
}

impl ApolloTraceThrottle {
    pub(crate) fn new(config: ApolloTraceThrottleConfig) -> Self {
        match config {
            ApolloTraceThrottleConfig::RepresentativeTraces => {
                Self::RepresentativeTraces(RepresentativeTraceFilter::new())
            }
            ApolloTraceThrottleConfig::RateLimited => {
                Self::RateLimited(TraceRateLimiter::new(MAX_TRACES_PER_SECOND))
            }
        }
    }

    /// A stable label for the active mode, used as a metric attribute.
    pub(crate) fn mode_name(&self) -> &'static str {
        match self {
            Self::RepresentativeTraces(_) => "representative_traces",
            Self::RateLimited(_) => "rate_limited",
        }
    }

    /// Decide whether a complete trace should be exported to Apollo. `trace[0]` is the root span.
    pub(crate) fn should_keep(&self, trace: &[LightSpanData]) -> bool {
        match self {
            Self::RepresentativeTraces(filter) => filter.should_keep(trace),
            Self::RateLimited(limiter) => limiter.check(Instant::now()),
        }
    }
}

/// Keeps at most one representative trace per minute for each distinct combination of dimensions,
/// mirroring the engine-reports `CacheBasedTraceFilter`. The first trace seen for a key is kept and
/// the key is remembered; later traces sharing that key (within the same minute) are dropped.
#[derive(Debug)]
pub(crate) struct RepresentativeTraceFilter {
    seen: Mutex<LruCache<u64, ()>>,
}

impl RepresentativeTraceFilter {
    fn new() -> Self {
        Self {
            seen: Mutex::new(LruCache::new(
                NonZeroUsize::new(REPRESENTATIVE_CACHE_CAPACITY)
                    .expect("capacity is non-zero, qed"),
            )),
        }
    }

    fn should_keep(&self, trace: &[LightSpanData]) -> bool {
        let Some(root) = trace.first() else {
            return false;
        };
        let key = Self::trace_key(root, trace_has_errors(trace));

        let mut seen = self.seen.lock();
        // `get` (rather than `contains`) refreshes recency so a hot key isn't evicted mid-minute.
        if seen.get(&key).is_some() {
            false
        } else {
            seen.put(key, ());
            true
        }
    }

    /// Hash of the dimension combination that identifies a "representative" trace. Mirrors the
    /// `EngineTracesKafkaKey` used by engine reports, minus dimensions that are constant for a
    /// single router instance (graph id/variant, trace format).
    fn trace_key(root: &LightSpanData, has_errors: bool) -> u64 {
        let end_secs = root
            .end_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Error traces are spread across twelve 5-second sub-buckets within the minute, so up to
        // 12 representative error traces per minute get through (vs. 1 for non-error traces).
        let error_key: i8 = if has_errors {
            ((end_secs % 60) / 5) as i8
        } else {
            -1
        };

        let mut hasher = DefaultHasher::new();
        string_attr(&root.attributes, &APOLLO_PRIVATE_OPERATION_SIGNATURE).hash(&mut hasher);
        (end_secs / 60).hash(&mut hasher); // minute floor
        duration_bucket(trace_duration(root)).hash(&mut hasher);
        error_key.hash(&mut hasher);
        string_attr(&root.attributes, &OPERATION_TYPE).hash(&mut hasher);
        string_attr(&root.attributes, &OPERATION_SUBTYPE).hash(&mut hasher);
        string_attr(&root.attributes, &CLIENT_NAME_KEY).hash(&mut hasher);
        string_attr(&root.attributes, &CLIENT_VERSION_KEY).hash(&mut hasher);
        hasher.finish()
    }
}

/// A fixed-window rate limiter: allows up to `max_per_second` keeps in each rolling one-second
/// window (per router instance). The clock is passed in so tests can advance time without sleeping.
#[derive(Debug)]
pub(crate) struct TraceRateLimiter {
    max_per_second: u32,
    window: Mutex<RateWindow>,
}

#[derive(Debug)]
struct RateWindow {
    start: Instant,
    count: u32,
}

impl TraceRateLimiter {
    fn new(max_per_second: u32) -> Self {
        Self {
            max_per_second,
            window: Mutex::new(RateWindow {
                start: Instant::now(),
                count: 0,
            }),
        }
    }

    fn check(&self, now: Instant) -> bool {
        let mut window = self.window.lock();
        if now.duration_since(window.start) >= Duration::from_secs(1) {
            window.start = now;
            window.count = 0;
        }
        if window.count < self.max_per_second {
            window.count += 1;
            true
        } else {
            false
        }
    }
}

/// Extract a string attribute value, or an empty string if absent/non-string. An empty string is a
/// stable, distinct key component (an operation with no client name always hashes the same way).
fn string_attr(attributes: &HashMap<Key, Value>, key: &Key) -> String {
    match attributes.get(key) {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => String::new(),
    }
}

/// The wall-clock duration of the trace, preferring the `apollo_private.duration_ns` attribute the
/// router records on the request span, and falling back to the span's own start/end times.
fn trace_duration(root: &LightSpanData) -> Duration {
    match root.attributes.get(&APOLLO_PRIVATE_DURATION_NS_KEY) {
        Some(Value::I64(ns)) if *ns >= 0 => Duration::from_nanos(*ns as u64),
        _ => root
            .end_time
            .duration_since(root.start_time)
            .unwrap_or_default(),
    }
}

/// Whether any span in the trace signals an error, used only as a bucketing dimension (so error
/// traces are sampled at a higher rate). This is an approximation that avoids decoding FTV1: it
/// looks at OTel span status and error events, not subgraph errors encoded inside the FTV1 blob.
fn trace_has_errors(trace: &[LightSpanData]) -> bool {
    trace.iter().any(|span| {
        matches!(span.status, Status::Error { .. })
            || span.events.iter().any(|event| {
                event
                    .attributes
                    .keys()
                    .any(|key| key.as_str() == GRAPHQL_ERROR_EXT_CODE)
            })
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceId;

    use super::*;

    // A representative filter with a small capacity is fine for these tests; nothing here relies on
    // the production capacity.
    fn representative_filter() -> RepresentativeTraceFilter {
        RepresentativeTraceFilter {
            seen: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
        }
    }

    fn at_second(secs: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Build a root request span with the given dimension attributes and end time.
    fn root(
        signature: &str,
        client_name: &str,
        duration_ns: i64,
        end: std::time::SystemTime,
        status: Status,
    ) -> LightSpanData {
        let mut attributes: HashMap<Key, Value> = HashMap::new();
        attributes.insert(
            APOLLO_PRIVATE_OPERATION_SIGNATURE,
            Value::String(signature.to_string().into()),
        );
        attributes.insert(
            CLIENT_NAME_KEY,
            Value::String(client_name.to_string().into()),
        );
        attributes.insert(CLIENT_VERSION_KEY, Value::String("v1".into()));
        attributes.insert(OPERATION_TYPE, Value::String("query".into()));
        attributes.insert(APOLLO_PRIVATE_DURATION_NS_KEY, Value::I64(duration_ns));

        LightSpanData {
            trace_id: TraceId::from_bytes([1; 16]),
            span_id: SpanId::from_bytes([1; 8]),
            parent_span_id: SpanId::INVALID,
            span_kind: SpanKind::Server,
            name: Cow::from("request"),
            start_time: end,
            end_time: end,
            attributes,
            status,
            droppped_attribute_count: 0,
            events: Vec::new(),
        }
    }

    #[test]
    fn keeps_first_and_drops_duplicate_within_minute() {
        let filter = representative_filter();
        let end = at_second(600); // minute floor = 10
        let make = || vec![root("sigA", "web", 1_000_000, end, Status::Unset)];

        assert!(filter.should_keep(&make()), "first of a key is kept");
        assert!(
            !filter.should_keep(&make()),
            "duplicate within minute dropped"
        );
        assert!(!filter.should_keep(&make()), "still dropped");
    }

    #[test]
    fn distinguishes_key_dimensions() {
        let end = at_second(600);
        // Each variation from the baseline must be treated as a new representative.
        let baseline = || vec![root("sigA", "web", 1_000_000, end, Status::Unset)];

        // signature, client, latency bucket, next minute, and error status each form a new key.
        let variants = vec![
            vec![root("sigB", "web", 1_000_000, end, Status::Unset)],
            vec![root("sigA", "ios", 1_000_000, end, Status::Unset)],
            vec![root("sigA", "web", 1_000_000_000, end, Status::Unset)], // different latency bucket
            vec![root(
                "sigA",
                "web",
                1_000_000,
                at_second(660),
                Status::Unset,
            )], // next minute
            vec![root("sigA", "web", 1_000_000, end, Status::error("boom"))],
        ];

        for variant in variants {
            let filter = representative_filter();
            assert!(filter.should_keep(&baseline()));
            assert!(
                filter.should_keep(&variant),
                "a differing dimension must produce a new representative"
            );
        }
    }

    #[test]
    fn error_traces_get_one_representative_per_five_second_subbucket() {
        let filter = representative_filter();
        // Within the same minute (secs 600..=659, minute floor 10), error traces are keyed by a
        // 5-second sub-bucket, so up to 12 distinct error representatives get through.
        let mut kept = 0;
        for sec in 600..660 {
            let trace = vec![root(
                "sigA",
                "web",
                1_000_000,
                at_second(sec),
                Status::error("boom"),
            )];
            if filter.should_keep(&trace) {
                kept += 1;
            }
        }
        assert_eq!(kept, 12, "12 five-second sub-buckets in a minute");
    }

    #[test]
    fn detects_errors_from_span_status() {
        let error_trace = vec![root(
            "sigA",
            "web",
            1_000_000,
            at_second(600),
            Status::error("boom"),
        )];
        assert!(trace_has_errors(&error_trace));

        let ok_trace = vec![root(
            "sigA",
            "web",
            1_000_000,
            at_second(600),
            Status::Unset,
        )];
        assert!(!trace_has_errors(&ok_trace));
    }

    #[test]
    fn rate_limiter_caps_per_window_and_resets() {
        let limiter = TraceRateLimiter::new(3);
        let base = Instant::now();

        assert!(limiter.check(base));
        assert!(limiter.check(base));
        assert!(limiter.check(base));
        assert!(!limiter.check(base), "4th in the same window is dropped");
        assert!(
            !limiter.check(base + Duration::from_millis(999)),
            "still same window"
        );

        assert!(
            limiter.check(base + Duration::from_secs(1)),
            "new window resets the count"
        );
    }
}
