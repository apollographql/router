//! Volume-reduction strategies for traces exported to Apollo Studio.
//!
//! These only affect the Apollo trace pipeline (see [`super::apollo`]); the customer OTLP and
//! Datadog pipelines and the global head sampler are untouched. Both strategies make a keep/drop
//! decision on a complete, reassembled trace, so they run inside
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
use crate::plugins::telemetry::consts::EXECUTION_SPAN_NAME;
use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
use crate::plugins::telemetry::metrics::apollo::histogram::duration_bucket;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_DURATION_NS_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_NAME_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::CLIENT_VERSION_KEY;
use crate::plugins::telemetry::tracing::apollo_telemetry::GRAPHQL_ERROR_EXT_CODE;
use crate::plugins::telemetry::tracing::apollo_telemetry::LightSpanData;
use crate::plugins::telemetry::tracing::apollo_telemetry::OPERATION_SUBTYPE;
use crate::plugins::telemetry::tracing::apollo_telemetry::OPERATION_TYPE;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;

/// Maximum number of distinct dimension combinations remembered by the representative-traces
/// filter. Because the trace's end-minute is part of every key, entries for elapsed minutes can
/// never match again and are evicted under LRU pressure, so this only needs to be large enough to
/// hold one minute's worth of distinct combinations for a busy graph. 50k entries equates to
/// about 2.5mb of memory usage.
const REPRESENTATIVE_CACHE_CAPACITY: usize = 50_000;

/// Fixed per-instance cap on traces exported per second in `rate_limited` mode. Deliberately not
/// user-configurable.
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

    /// Decide whether a complete trace should be exported to Apollo, given the summary gathered
    /// from its spans by [`TraceSummary::from_trace`].
    pub(crate) fn should_keep(&self, summary: &TraceSummary) -> bool {
        match self {
            Self::RepresentativeTraces(filter) => filter.should_keep(summary),
            Self::RateLimited(limiter) => limiter.check(Instant::now()),
        }
    }
}

/// Everything the Apollo export path needs to know about a complete, reassembled trace, gathered
/// in a single pass over its spans: the approximate serialized size (for the hard size limit)
/// and the dimensions identifying a representative trace (for the throttle).
///
/// The key dimensions are spread across different spans:
///
/// | Dimension                | Span it is recorded on                               |
/// |--------------------------|------------------------------------------------------|
/// | operation signature      | `supergraph`, or the `subscription_event` root       |
/// | operation type / subtype | `execution` (also on the `router` root for the type) |
/// | client name / version    | root (`router`) span                                 |
/// | duration, end time       | root span                                            |
/// | errors                   | any span                                             |
#[derive(Debug, Default)]
pub(crate) struct TraceSummary {
    /// Approximate serialized size of the whole trace, in bytes.
    pub(crate) approx_size_bytes: usize,
    /// Whether or not Apollo's exporter will actually send this trace. This mirrors functionality
    /// that was originally present in `ApolloOtlpExporter::prepare_for_export`: A trace is only
    /// sent if it contains a `supergraph` span carrying the `apollo_private.operation_signature`
    /// key (the value may be empty). In practice this excludes introspection queries, and also
    /// any trace with no supergraph span at all. Checking it up front lets the exporter discard
    /// such traces before they consume a representative-trace slot or a rate-limit token.
    /// Note this is presence-only, whereas [`Self::signature`] additionally requires a non-empty
    /// value: a trace with an empty signature is still exported, it just cannot be deduplicated.
    is_exportable: bool,
    /// `None` when the trace carries no usable operation signature, which means no key can be
    /// computed and the trace must not be deduplicated.
    signature: Option<String>,
    operation_type: String,
    operation_subtype: String,
    client_name: String,
    client_version: String,
    duration: Duration,
    end_time_secs: u64,
    has_errors: bool,
}

impl TraceSummary {
    /// Walk the trace's spans once, accumulating size and picking each key dimension off
    /// whichever span actually carries it.
    pub(crate) fn from_trace(trace: &[LightSpanData]) -> Self {
        let mut summary = TraceSummary::default();
        // The signature is preferentially taken from the supergraph span, falling back to a
        // subscription-event root.
        let mut supergraph_signature: Option<String> = None;
        let mut subscription_signature: Option<String> = None;

        for span in trace {
            summary.approx_size_bytes += approx_span_size(span);
            summary.has_errors |= span_has_errors(span);

            match span.name.as_ref() {
                SUPERGRAPH_SPAN_NAME => {
                    // This mirrors a check that used to live inside ApolloOtlpExporter::prepare_for_export
                    summary.is_exportable |= span
                        .attributes
                        .contains_key(&APOLLO_PRIVATE_OPERATION_SIGNATURE);
                    supergraph_signature =
                        non_empty_attr(&span.attributes, &APOLLO_PRIVATE_OPERATION_SIGNATURE);
                }
                SUBSCRIPTION_EVENT_SPAN_NAME => {
                    subscription_signature =
                        non_empty_attr(&span.attributes, &APOLLO_PRIVATE_OPERATION_SIGNATURE);
                }
                EXECUTION_SPAN_NAME => {
                    summary.operation_type = string_attr(&span.attributes, &OPERATION_TYPE);
                    summary.operation_subtype = string_attr(&span.attributes, &OPERATION_SUBTYPE);
                }
                _ => {}
            }
        }

        summary.signature = supergraph_signature.or(subscription_signature);

        // Duration, end time and the client attributes come from the root span.
        if let Some(root) = trace.first() {
            summary.duration = trace_duration(root);
            summary.end_time_secs = root
                .end_time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            summary.client_name = string_attr(&root.attributes, &CLIENT_NAME_KEY);
            summary.client_version = string_attr(&root.attributes, &CLIENT_VERSION_KEY);
            // The router span also records the operation type; use it when there was no execution
            // span in the trace (e.g. a request that failed before execution).
            if summary.operation_type.is_empty() {
                summary.operation_type = string_attr(&root.attributes, &OPERATION_TYPE);
            }
        }

        summary
    }

    pub(crate) fn is_exportable(&self) -> bool {
        self.is_exportable
    }

    /// Hash of the dimension combination that identifies a "representative" trace, or `None` if
    /// data required to build the key is missing.
    fn trace_key(&self) -> Option<u64> {
        // Signature is the only string that is required to build a trace key. Client name/version
        // and operation subtype are legitimately absent in some traces.
        let signature = self.signature.as_ref()?;

        // Error traces are spread across twelve 5-second sub-buckets within the minute, so up to
        // 12 representative error traces per minute get through (vs. 1 for non-error traces).
        let error_key: i8 = if self.has_errors {
            ((self.end_time_secs % 60) / 5) as i8
        } else {
            -1
        };

        let mut hasher = DefaultHasher::new();
        signature.hash(&mut hasher);
        (self.end_time_secs / 60).hash(&mut hasher); // minute floor
        duration_bucket(self.duration).hash(&mut hasher);
        error_key.hash(&mut hasher);
        self.operation_type.hash(&mut hasher);
        self.operation_subtype.hash(&mut hasher);
        self.client_name.hash(&mut hasher);
        self.client_version.hash(&mut hasher);
        Some(hasher.finish())
    }
}

/// Keeps at most one representative trace per minute for each distinct combination of dimensions.
/// The first trace seen for a key is kept and the key is remembered so later traces sharing that
/// key (within the same minute) are dropped.
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

    fn should_keep(&self, summary: &TraceSummary) -> bool {
        // Fail open: if the key can't be built we must not deduplicate, because an incomplete key
        // would conflate unrelated traces and silently drop them.
        let Some(key) = summary.trace_key() else {
            return true;
        };

        let mut seen = self.seen.lock();
        // `get` (rather than `contains`) refreshes recency so a hot key isn't evicted mid-minute.
        if seen.get(&key).is_some() {
            false
        } else {
            seen.put(key, ());
            true
        }
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

/// Extract a string attribute, or `None` when it is absent, not a string, or empty — all of which
/// mean "we don't know this dimension" rather than "this dimension is the empty string".
fn non_empty_attr(attributes: &HashMap<Key, Value>, key: &Key) -> Option<String> {
    match attributes.get(key) {
        Some(Value::String(s)) if !s.as_str().is_empty() => Some(s.as_str().to_string()),
        _ => None,
    }
}

/// Whether a span signals an error, used only as a bucketing dimension (so error traces are
/// sampled at a higher rate). This is an approximation that avoids decoding FTV1: it looks at OTel
/// span status and error events, not subgraph errors encoded inside the FTV1 blob.
fn span_has_errors(span: &LightSpanData) -> bool {
    matches!(span.status, Status::Error { .. })
        || span.events.iter().any(|event| {
            event
                .attributes
                .keys()
                .any(|key| key.as_str() == GRAPHQL_ERROR_EXT_CODE)
        })
}

/// Approximate serialized size of a span. This counts the dominant contributors — string
/// attribute/event values (e.g. large FTV1 blobs) and names — and ignores the small,
/// roughly-fixed per-span framing, which is enough for a coarse size guard.
fn approx_span_size(span: &LightSpanData) -> usize {
    let attributes: usize = span
        .attributes
        .iter()
        .map(|(k, v)| k.as_str().len() + attribute_value_size(v))
        .sum();
    let events: usize = span
        .events
        .iter()
        .map(|event| {
            event.name.len()
                + event
                    .attributes
                    .iter()
                    .map(|(k, v)| k.as_str().len() + attribute_value_size(v))
                    .sum::<usize>()
        })
        .sum();
    span.name.len() + attributes + events
}

fn attribute_value_size(value: &Value) -> usize {
    match value {
        Value::String(s) => s.as_str().len(),
        // Scalars and (rare) array attributes contribute negligibly next to the large string
        // blobs (e.g. FTV1) that are what actually push a trace over the limit.
        _ => 8,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceId;

    use super::*;
    use crate::plugins::telemetry::consts::EXECUTION_SPAN_NAME;
    use crate::plugins::telemetry::consts::ROUTER_SPAN_NAME;
    use crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME;
    use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;

    // A representative filter with a small capacity is fine for these tests; nothing here relies on
    // the production capacity.
    fn representative_filter() -> RepresentativeTraceFilter {
        RepresentativeTraceFilter {
            seen: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
        }
    }

    /// Build the summary the way `export` does, then ask the filter. Tests operate on real span
    /// trees so they exercise the same extraction path as production.
    fn keep(filter: &RepresentativeTraceFilter, trace: &[LightSpanData]) -> bool {
        filter.should_keep(&TraceSummary::from_trace(trace))
    }

    fn at_second(secs: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// A bare span with the given name; callers add whichever attributes the test needs. Tests
    /// deliberately do not put every dimension on one span, because the router never does.
    fn named_span(name: &'static str, end: std::time::SystemTime) -> LightSpanData {
        LightSpanData {
            trace_id: TraceId::from_bytes([1; 16]),
            span_id: SpanId::from_bytes([1; 8]),
            parent_span_id: SpanId::INVALID,
            span_kind: SpanKind::Server,
            name: Cow::from(name),
            start_time: end,
            end_time: end,
            attributes: HashMap::new(),
            status: Status::Unset,
            droppped_attribute_count: 0,
            events: Vec::new(),
        }
    }

    /// Build a trace with the span topology the router actually produces: the root `router` span
    /// carries `apollo_private.request`, the duration and the client attributes; the `supergraph`
    /// span carries the operation signature; and the `execution` span carries the operation type.
    /// No single span carries all of the key dimensions.
    fn realistic_trace(
        signature: &str,
        client_name: &str,
        duration_ns: i64,
        end: std::time::SystemTime,
        status: Status,
    ) -> Vec<LightSpanData> {
        let mut router = named_span(ROUTER_SPAN_NAME, end);
        router.status = status;
        router.attributes.insert(
            CLIENT_NAME_KEY,
            Value::String(client_name.to_string().into()),
        );
        router
            .attributes
            .insert(CLIENT_VERSION_KEY, Value::String("v1".into()));
        router
            .attributes
            .insert(APOLLO_PRIVATE_DURATION_NS_KEY, Value::I64(duration_ns));

        let mut supergraph = named_span(SUPERGRAPH_SPAN_NAME, end);
        supergraph.attributes.insert(
            APOLLO_PRIVATE_OPERATION_SIGNATURE,
            Value::String(signature.to_string().into()),
        );

        let mut execution = named_span(EXECUTION_SPAN_NAME, end);
        execution
            .attributes
            .insert(OPERATION_TYPE, Value::String("query".into()));

        vec![router, supergraph, execution]
    }

    /// The same topology, but the supergraph span's signature is present and empty. The trace is
    /// still exportable, yet no dedup key can be built from it, so the throttle must fail open.
    fn trace_with_empty_signature(end: std::time::SystemTime) -> Vec<LightSpanData> {
        realistic_trace("", "web", 1_000_000, end, Status::Unset)
    }

    /// A trace whose supergraph span has no signature attribute at all. Apollo's exporter drops
    /// these, so they should never reach the throttle.
    fn unexportable_trace(end: std::time::SystemTime) -> Vec<LightSpanData> {
        let mut trace = realistic_trace("ignored", "web", 1_000_000, end, Status::Unset);
        for span in &mut trace {
            span.attributes.remove(&APOLLO_PRIVATE_OPERATION_SIGNATURE);
        }
        trace
    }

    /// Mirrors code that was previously in `ApolloOtlpExporter::prepare_for_export`: a trace is
    /// only sent when a `supergraph` span carries the signature key, so traces without one must
    /// be reported as unexportable and skipped before they can consume a throttle slot.
    #[test]
    fn traces_without_a_signature_bearing_supergraph_span_are_unexportable() {
        let end = at_second(600);

        assert!(
            TraceSummary::from_trace(&realistic_trace(
                "sigA",
                "web",
                1_000_000,
                end,
                Status::Unset
            ))
            .is_exportable()
        );
        assert!(
            !TraceSummary::from_trace(&unexportable_trace(end)).is_exportable(),
            "no signature attribute on the supergraph span means Apollo drops the trace"
        );
        // Presence of the key is what counts, not a non-empty value.
        assert!(
            TraceSummary::from_trace(&trace_with_empty_signature(end)).is_exportable(),
            "an empty signature is still exported"
        );
    }

    /// A subscription-event trace has no supergraph span, so Apollo's exporter discards it. It
    /// must therefore be reported as unexportable rather than consuming a throttle slot.
    #[test]
    fn subscription_event_traces_without_a_supergraph_span_are_unexportable() {
        let end = at_second(600);
        let mut event = named_span(SUBSCRIPTION_EVENT_SPAN_NAME, end);
        event.attributes.insert(
            APOLLO_PRIVATE_OPERATION_SIGNATURE,
            Value::String("subSig".into()),
        );

        assert!(!TraceSummary::from_trace(&[event]).is_exportable());
    }

    /// Regression test for the key being read only from the root span: the operation signature
    /// lives on the `supergraph` span, so identical traces were never recognised as duplicates.
    #[test]
    fn dedups_traces_whose_signature_lives_on_the_supergraph_span() {
        let filter = representative_filter();
        let end = at_second(600);
        let make = || realistic_trace("sigA", "web", 1_000_000, end, Status::Unset);

        assert!(keep(&filter, &make()), "first trace is kept");
        assert!(
            !keep(&filter, &make()),
            "a duplicate of the same operation within the minute must be dropped"
        );
    }

    /// The flip side: distinct operations must not be conflated. Without reading the supergraph
    /// span, every operation shares an (empty) signature and collapses into one representative.
    #[test]
    fn distinguishes_operations_by_signature_on_the_supergraph_span() {
        let filter = representative_filter();
        let end = at_second(600);

        assert!(keep(
            &filter,
            &realistic_trace("sigA", "web", 1_000_000, end, Status::Unset)
        ));
        assert!(
            keep(
                &filter,
                &realistic_trace("sigB", "web", 1_000_000, end, Status::Unset)
            ),
            "a different operation must be kept, not treated as a duplicate"
        );
    }

    /// Covers the signature falling back to the `subscription_event` root, which carries it
    /// directly. Note such traces have no supergraph span, so `export` discards them as
    /// unexportable before the throttle ever sees them today.
    #[test]
    fn uses_the_subscription_event_signature_when_there_is_no_supergraph_span() {
        let filter = representative_filter();
        let end = at_second(600);
        let make = || {
            let mut event = named_span(SUBSCRIPTION_EVENT_SPAN_NAME, end);
            event.attributes.insert(
                APOLLO_PRIVATE_OPERATION_SIGNATURE,
                Value::String("subSig".into()),
            );
            event
                .attributes
                .insert(APOLLO_PRIVATE_DURATION_NS_KEY, Value::I64(1_000_000));
            vec![event]
        };

        assert!(keep(&filter, &make()), "first event trace is kept");
        assert!(
            !keep(&filter, &make()),
            "duplicate subscription event must be dropped"
        );
    }

    #[test]
    fn keeps_every_trace_when_the_operation_signature_is_missing() {
        let filter = representative_filter();
        let end = at_second(600);

        // Identical traces that would otherwise dedup to a single representative: without the
        // signature we cannot tell operations apart, so all of them must be kept. `export` now
        // discards these even earlier as unexportable; the filter fails open regardless.
        for i in 0..5 {
            assert!(
                keep(&filter, &unexportable_trace(end)),
                "trace {i} must be kept when the key cannot be computed"
            );
        }
    }

    #[test]
    fn keeps_every_trace_when_the_operation_signature_is_empty() {
        let filter = representative_filter();
        let end = at_second(600);
        let make = || realistic_trace("", "web", 1_000_000, end, Status::Unset);

        // An empty signature means "unknown", not "an operation whose signature is the empty
        // string", so it must fail open just like an absent attribute.
        assert!(keep(&filter, &make()));
        assert!(keep(&filter, &make()));
    }

    #[test]
    fn missing_signature_does_not_poison_the_cache_for_valid_traces() {
        let filter = representative_filter();
        let end = at_second(600);

        // Fail-open traces must not insert a key, or they would evict/collide with real ones.
        assert!(keep(&filter, &unexportable_trace(end)));
        assert!(keep(&filter, &unexportable_trace(end)));

        // A normal trace still dedups as usual.
        let valid = || realistic_trace("sigA", "web", 1_000_000, end, Status::Unset);
        assert!(keep(&filter, &valid()), "first valid trace is kept");
        assert!(
            !keep(&filter, &valid()),
            "duplicate valid trace is still dropped"
        );
    }

    #[test]
    fn keeps_first_and_drops_duplicate_within_minute() {
        let filter = representative_filter();
        let end = at_second(600); // minute floor = 10
        let make = || realistic_trace("sigA", "web", 1_000_000, end, Status::Unset);

        assert!(keep(&filter, &make()), "first of a key is kept");
        assert!(!keep(&filter, &make()), "duplicate within minute dropped");
        assert!(!keep(&filter, &make()), "still dropped");
    }

    #[test]
    fn distinguishes_key_dimensions() {
        let end = at_second(600);
        // Each variation from the baseline must be treated as a new representative.
        let baseline = || realistic_trace("sigA", "web", 1_000_000, end, Status::Unset);

        // signature, client, latency bucket, next minute, and error status each form a new key.
        let variants = vec![
            realistic_trace("sigB", "web", 1_000_000, end, Status::Unset),
            realistic_trace("sigA", "ios", 1_000_000, end, Status::Unset),
            realistic_trace("sigA", "web", 1_000_000_000, end, Status::Unset), // different latency bucket
            realistic_trace("sigA", "web", 1_000_000, at_second(660), Status::Unset), // next minute
            realistic_trace("sigA", "web", 1_000_000, end, Status::error("boom")),
        ];

        for variant in variants {
            let filter = representative_filter();
            assert!(keep(&filter, &baseline()));
            assert!(
                keep(&filter, &variant),
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
            let trace = realistic_trace(
                "sigA",
                "web",
                1_000_000,
                at_second(sec),
                Status::error("boom"),
            );
            if keep(&filter, &trace) {
                kept += 1;
            }
        }
        assert_eq!(kept, 12, "12 five-second sub-buckets in a minute");
    }

    #[test]
    fn detects_errors_from_span_status() {
        let error_trace = realistic_trace(
            "sigA",
            "web",
            1_000_000,
            at_second(600),
            Status::error("boom"),
        );
        assert!(TraceSummary::from_trace(&error_trace).has_errors);

        let ok_trace = realistic_trace("sigA", "web", 1_000_000, at_second(600), Status::Unset);
        assert!(!TraceSummary::from_trace(&ok_trace).has_errors);
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
