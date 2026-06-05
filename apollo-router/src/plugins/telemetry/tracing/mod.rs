use std::fmt::Display;
use std::fmt::Formatter;
use std::time::Duration;

use opentelemetry::{Context, TraceId};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::BatchConfig;
use opentelemetry_sdk::trace::BatchConfigBuilder;
use opentelemetry_sdk::trace::Span;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanProcessor;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use super::formatters::APOLLO_CONNECTOR_PREFIX;
use super::formatters::APOLLO_PRIVATE_PREFIX;
use crate::plugins::telemetry::config::Sampler;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::tracing::datadog::DatadogSpanProcessor;

pub(crate) mod apollo;
pub(crate) mod apollo_telemetry;
pub(crate) mod datadog;
#[allow(unreachable_pub, dead_code)]
pub(crate) mod datadog_exporter;
mod named;
pub(crate) mod otlp;
pub(crate) mod reload;
pub(crate) mod zipkin;

pub(crate) use named::NamedSpanExporter;
pub(crate) use named::NamedTokioRuntime;

#[derive(Debug)]
struct ApolloFilterSpanProcessor<T: SpanProcessor> {
    delegate: T,
}

impl<T: SpanProcessor> SpanProcessor for ApolloFilterSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx);
    }

    fn on_end(&self, span: SpanData) {
        if span.attributes.iter().any(|kv| {
            kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                || kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
        }) {
            let span = SpanData {
                attributes: span
                    .attributes
                    .into_iter()
                    .filter(|kv| {
                        !kv.key.as_str().starts_with(APOLLO_PRIVATE_PREFIX)
                            && !kv.key.as_str().starts_with(APOLLO_CONNECTOR_PREFIX)
                    })
                    .collect(),
                ..span
            };

            self.delegate.on_end(span);
        } else {
            self.delegate.on_end(span);
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.delegate.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.delegate.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.delegate.set_resource(resource)
    }
}

/// A span processor that applies a secondary sampling filter based on the trace ID.
///
/// Uses the same algorithm as OTel's `TraceIdRatioBased` sampler (low 64 bits, right-shifted
/// by 1, compared against a threshold scaled to `[0, 2^63)`), so per-exporter ratios represent
/// absolute fractions of all requests — not fractions of already-sampled spans.
///
/// ## Parent-based sampling interaction
///
/// When `parent_based_sampler: true` (the default), OTel's `ParentBased` sampler has two paths:
///
/// 1. **Root spans / unsampled parent**: delegates to `TraceIdRatioBased` — hash-based decision.
/// 2. **Upstream-sampled spans** (incoming `traceparent` with the SAMPLED flag set): always
///    samples, overriding the hash check.
///
/// A span arriving via path 2 may have a trace ID whose hash falls outside the per-exporter
/// threshold, yet it legitimately reached `on_end`. To avoid silently discarding these spans,
/// `SamplingSpanProcessor` tracks the global threshold (as `Some(threshold)` when
/// `parent_based_sampler` is true, `None` otherwise): if a span's hash is outside the global
/// threshold (only possible via a parent-flag override), it is always forwarded regardless of
/// the per-exporter threshold.
///
/// When `parent_based_sampler: false`, the parent-override branch is never taken because the
/// global sampler drops spans with hash >= global_threshold before they reach `on_end`.
#[derive(Debug)]
pub(crate) struct SamplingSpanProcessor<T: SpanProcessor> {
    delegate: T,

    threshold: u64,

    /// Global common sampler, used to detect parent-flag overrides.
    /// Only set if parent_based_sampler is true (ie overrides are allowed)
    global_threshold: Option<u64>,
}

fn threshold_from_sampler_option(sampler_option: &SamplerOption) -> u64 {
    match sampler_option {
        SamplerOption::Always(Sampler::AlwaysOn) => u64::MAX,
        SamplerOption::Always(Sampler::AlwaysOff) => u64::MIN,
        SamplerOption::TraceIdRatioBased(ratio) => {
            let ratio = ratio.clamp(0.0, 1.0);

            // uses 2^63 as max rather than 2^64 to mirror OTel SDK's TraceIdRatioBased sampler
            let threshold = ratio * (1u64 << 63) as f64;
            threshold as u64
        }

    }
}

impl<T: SpanProcessor> SamplingSpanProcessor<T> {
    pub(crate) fn new(
        delegate: T,
        sampler: &SamplerOption,
        parent_based_sampler: bool,
        global_sampler: &SamplerOption,
    ) -> Self {
        Self {
            delegate,
            threshold: threshold_from_sampler_option(sampler),
            global_threshold: parent_based_sampler.then_some(threshold_from_sampler_option(global_sampler))
        }
    }
}

impl<T: SpanProcessor> SpanProcessor for SamplingSpanProcessor<T> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.delegate.on_start(span, cx);
    }

    fn on_end(&self, span: SpanData) {
        let low_bits = trace_id_to_low_bits(span.span_context.trace_id());

        // Forward if within per-exporter threshold (normal sub-sampling), OR if above the global
        // threshold (only reachable when ParentBased honored an upstream SAMPLED flag — preserve
        // that decision). Spans between the two thresholds are legitimately sub-sampled: drop.
        if low_bits < self.threshold || self.global_threshold.is_some_and(|t| low_bits >= t) {
            self.delegate.on_end(span);
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.delegate.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.delegate.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.delegate.set_resource(resource)
    }
}

/// Extracts the sampling-relevant bits from a trace ID, mirroring OTel SDK's `TraceIdRatioBased`.
///
/// Takes the low 64 bits (big-endian bytes 8–15) and right-shifts by 1 to produce a value in
/// `[0, 2^63)`. The right shift avoids f64 precision issues when scaling thresholds: `2^63` is
/// exactly representable as f64, whereas `u64::MAX` is not (it rounds up to `2^64`, causing
/// overflow when cast back). The threshold is precomputed as `ratio * 2^63`.
fn trace_id_to_low_bits(trace_id: TraceId) -> u64 {
    let bytes: [u8; 16] = trace_id.to_bytes();
    u64::from_be_bytes(bytes[8..].try_into().unwrap()) >> 1
}

trait SpanProcessorExt
where
    Self: Sized + SpanProcessor,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self>;
    fn always_sampled(self) -> DatadogSpanProcessor<Self>;
    fn with_sampler(
        self,
        sampler: &SamplerOption,
        parent_based_sampler: bool,
        global_sampler: &SamplerOption,
    ) -> SamplingSpanProcessor<Self>;
}

impl<T: SpanProcessor> SpanProcessorExt for T
where
    Self: Sized,
{
    fn filtered(self) -> ApolloFilterSpanProcessor<Self> {
        ApolloFilterSpanProcessor { delegate: self }
    }

    /// This span processor will always send spans to the exporter even if they are not sampled. This is useful for the datadog agent which
    /// uses spans for metrics.
    fn always_sampled(self) -> DatadogSpanProcessor<Self> {
        DatadogSpanProcessor::new(self)
    }

    fn with_sampler(
        self,
        sampler: &SamplerOption,
        parent_based_sampler: bool,
        global_sampler: &SamplerOption,
    ) -> SamplingSpanProcessor<Self> {
        SamplingSpanProcessor::new(self, sampler, parent_based_sampler, global_sampler)
    }
}

/// Batch processor configuration
#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub(crate) struct BatchProcessorConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    /// The delay interval in milliseconds between two consecutive processing
    /// of batches. The default value is 5 seconds.
    pub(crate) scheduled_delay: Duration,

    /// The maximum queue size to buffer spans for delayed processing. If the
    /// queue gets full it drops the spans. The default value is 2048.
    pub(crate) max_queue_size: usize,

    /// The maximum number of spans to process in a single batch. If there are
    /// more than one batch worth of spans then it processes multiple batches
    /// of spans one batch after the other without any delay. The default value
    /// is 512.
    pub(crate) max_export_batch_size: usize,

    /// The maximum duration to export a batch of data.
    /// The default value is 30 seconds.
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(crate) max_export_timeout: Duration,

    /// Maximum number of concurrent exports
    ///
    /// Limits the number of spawned tasks for exports and thus memory consumed
    /// by an exporter. A value of 1 will cause exports to be performed
    /// synchronously on the BatchSpanProcessor task.
    /// The default is 1.
    pub(crate) max_concurrent_exports: usize,
}

pub(crate) fn scheduled_delay_default() -> Duration {
    Duration::from_secs(5)
}

pub(crate) fn max_queue_size_default() -> usize {
    2048
}

fn max_export_batch_size_default() -> usize {
    512
}

pub(crate) fn max_export_timeout_default() -> Duration {
    Duration::from_secs(30)
}

fn max_concurrent_exports_default() -> usize {
    1
}

impl BatchProcessorConfig {
    /// Apply OTEL_BSP_* environment variable overrides to this config.
    /// This should be used for third-party exporters (OTLP, Datadog, Zipkin)
    /// but NOT for Apollo exporters.
    pub(crate) fn with_env_overrides(self) -> Result<Self, BoxError> {
        Ok(BatchProcessorConfig {
            scheduled_delay: Self::parse_duration_env(
                "OTEL_BSP_SCHEDULE_DELAY",
                self.scheduled_delay,
            )?,
            max_queue_size: Self::parse_usize_env("OTEL_BSP_MAX_QUEUE_SIZE", self.max_queue_size)?,
            max_export_batch_size: Self::parse_usize_env(
                "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
                self.max_export_batch_size,
            )?,
            max_export_timeout: Self::parse_duration_env(
                "OTEL_BSP_EXPORT_TIMEOUT",
                self.max_export_timeout,
            )?,
            max_concurrent_exports: Self::parse_usize_env(
                "OTEL_BSP_MAX_CONCURRENT_EXPORTS",
                self.max_concurrent_exports,
            )?,
        })
    }

    fn parse_duration_env(var: &str, default: Duration) -> Result<Duration, BoxError> {
        match std::env::var(var) {
            Ok(value) => {
                let millis = value.parse::<u64>().map_err(|e| {
                    format!(
                        "invalid value '{}' for {}, expected milliseconds: {}",
                        value, var, e
                    )
                })?;
                Ok(Duration::from_millis(millis))
            }
            Err(_) => Ok(default),
        }
    }

    fn parse_usize_env(var: &str, default: usize) -> Result<usize, BoxError> {
        match std::env::var(var) {
            Ok(value) => value.parse::<usize>().map_err(|e| {
                format!(
                    "invalid value '{}' for {}, expected integer: {}",
                    value, var, e
                )
                .into()
            }),
            Err(_) => Ok(default),
        }
    }
}

impl From<BatchProcessorConfig> for BatchConfig {
    fn from(config: BatchProcessorConfig) -> Self {
        BatchConfigBuilder::default()
            .with_scheduled_delay(config.scheduled_delay)
            .with_max_queue_size(config.max_queue_size)
            .with_max_export_batch_size(config.max_export_batch_size)
            // Concurrent exports and export timeout require experimental_trace_batch_span_processor_with_async_runtime feature
            .with_max_concurrent_exports(config.max_concurrent_exports)
            .with_max_export_timeout(config.max_export_timeout)
            .build()
    }
}

impl Display for BatchProcessorConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("BatchConfig {{ scheduled_delay={}, max_queue_size={}, max_export_batch_size={}, max_export_timeout={}, max_concurrent_exports={} }}",
                             humantime::format_duration(self.scheduled_delay),
                             self.max_queue_size,
                             self.max_export_batch_size,
                             humantime::format_duration(self.max_export_timeout),
                             self.max_concurrent_exports))
    }
}

impl Default for BatchProcessorConfig {
    fn default() -> Self {
        BatchProcessorConfig {
            scheduled_delay: scheduled_delay_default(),
            max_queue_size: max_queue_size_default(),
            max_export_batch_size: max_export_batch_size_default(),
            max_export_timeout: max_export_timeout_default(),
            max_concurrent_exports: max_concurrent_exports_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::SystemTime;

    use opentelemetry::InstrumentationScope;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::Status;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanLinks;

    use super::*;

    fn make_span(trace_id: u128) -> SpanData {
        SpanData {
            span_context: SpanContext::new(
                TraceId::from(trace_id),
                SpanId::from(1u64),
                TraceFlags::default().with_sampled(true),
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "test".into(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: SystemTime::UNIX_EPOCH,
            attributes: vec![],
            events: Default::default(),
            links: SpanLinks::default(),
            status: Status::Unset,
            instrumentation_scope: InstrumentationScope::default(),
            dropped_attributes_count: 0,
        }
    }

    /// A span processor that records every span it receives.
    #[derive(Clone, Default, Debug)]
    struct RecordingProcessor(Arc<Mutex<Vec<SpanData>>>);

    impl SpanProcessor for RecordingProcessor {
        fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &Context) {}
        fn on_end(&self, span: SpanData) {
            self.0.lock().unwrap().push(span);
        }
        fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
        fn shutdown_with_timeout(
            &self,
            _timeout: Duration,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
    }

    fn make_processor(
        recorder: RecordingProcessor,
        sampler: SamplerOption,
        parent_based_sampler: bool,
        global: SamplerOption,
    ) -> SamplingSpanProcessor<RecordingProcessor> {
        SamplingSpanProcessor::new(recorder, &sampler, parent_based_sampler, &global)
    }

    #[test]
    fn always_on_forwards_all_spans() {
        let recorder = RecordingProcessor::default();
        let processor = make_processor(
            recorder.clone(),
            SamplerOption::Always(Sampler::AlwaysOn),
            true,
            SamplerOption::Always(Sampler::AlwaysOn),
        );
        for i in 0u128..20 {
            processor.on_end(make_span(i));
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 20);
    }

    #[test]
    fn always_off_drops_all_spans() {
        let recorder = RecordingProcessor::default();
        let processor = make_processor(
            recorder.clone(),
            SamplerOption::Always(Sampler::AlwaysOff),
            true,
            SamplerOption::Always(Sampler::AlwaysOn),
        );
        for i in 0u128..20 {
            processor.on_end(make_span(i));
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 0);
    }

    #[test]
    fn ratio_based_is_deterministic() {
        let sampler = SamplerOption::TraceIdRatioBased(0.5);
        let global = SamplerOption::Always(Sampler::AlwaysOn);

        // Trace ID 42: low 8 bytes = [0,0,0,0,0,0,0,42], raw_low=42, low_bits=21.
        // threshold(0.5) = (0.5 * 2^63) ≈ 4.6e18. 21 < threshold → this span is FORWARDED.
        // The assertion below pins both processors making the same forward decision.
        let recorder1 = RecordingProcessor::default();
        let p1 = make_processor(recorder1.clone(), sampler.clone(), true,global.clone());
        p1.on_end(make_span(42));

        let recorder2 = RecordingProcessor::default();
        let p2 = make_processor(recorder2.clone(), sampler,true, global);
        p2.on_end(make_span(42));

        assert_eq!(
            recorder1.0.lock().unwrap().len(),
            recorder2.0.lock().unwrap().len(),
            "same trace ID must produce the same sampling decision"
        );
    }

    #[test]
    fn ratio_zero_drops_all_spans() {
        let recorder = RecordingProcessor::default();
        let processor = make_processor(
            recorder.clone(),
            SamplerOption::TraceIdRatioBased(0.0),
            true,
            SamplerOption::Always(Sampler::AlwaysOn),
        );
        for i in 0u128..20 {
            processor.on_end(make_span(i));
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 0);
    }

    #[test]
    fn ratio_one_forwards_all_spans() {
        let recorder = RecordingProcessor::default();
        let processor = make_processor(
            recorder.clone(),
            SamplerOption::TraceIdRatioBased(1.0),
            true,
            SamplerOption::Always(Sampler::AlwaysOn),
        );
        for i in 0u128..20 {
            processor.on_end(make_span(i));
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 20);
    }

    /// AlwaysOff still passes through parent-flag overrides, consistent with how the common
    /// sampler works: if the calling service has already decided to sample a trace, that
    /// decision is respected regardless of the per-exporter threshold.
    #[test]
    fn always_off_passes_through_parent_based_override_spans() {
        let global = SamplerOption::TraceIdRatioBased(0.5);
        let per_exporter = SamplerOption::Always(Sampler::AlwaysOff);

        let recorder = RecordingProcessor::default();
        let processor = make_processor(recorder.clone(), per_exporter, true, global);

        // Span with hash above global_threshold — only reachable via parent-flag override.
        let span = make_span(u64::MAX as u128);
        processor.on_end(span);

        assert_eq!(
            recorder.0.lock().unwrap().len(),
            1,
            "AlwaysOff should still pass through parent-flag override spans"
        );
    }

    /// A span whose trace ID hash falls outside the global threshold can only have reached
    /// on_end because ParentBased honored an upstream SAMPLED flag. It must be forwarded.
    #[test]
    fn parent_based_override_forwards_spans_outside_global_threshold() {
        // global = 0.5, per-exporter = 0.02
        // global_threshold = (0.5 * 2^63) as u64 ≈ 2^62
        // We need low_bits >= global_threshold, i.e. (raw_low >> 1) >= 2^62, i.e. raw_low >= 2^63.
        let global = SamplerOption::TraceIdRatioBased(0.5);
        let per_exporter = SamplerOption::TraceIdRatioBased(0.02);

        let recorder = RecordingProcessor::default();
        let processor = make_processor(recorder.clone(), per_exporter, true, global);

        // raw_low = u64::MAX → low_bits = u64::MAX >> 1 = 2^63 - 1, which is above global_threshold.
        let span = make_span(u64::MAX as u128);
        processor.on_end(span);

        assert_eq!(
            recorder.0.lock().unwrap().len(),
            1,
            "span with hash above global threshold must be forwarded — it arrived via parent-flag override"
        );
    }

    /// When `parent_based_sampler` is false, `global_threshold` is `None` and the parent-flag
    /// override branch is never taken. A span whose hash falls above the per-exporter threshold
    /// must be dropped even if it would have been a parent-flag override in parent-based mode.
    #[test]
    fn parent_based_off_does_not_forward_spans_outside_threshold() {
        // per-exporter = 0.02; parent_based_sampler = false → global_threshold = None
        let per_exporter = SamplerOption::TraceIdRatioBased(0.02);
        let global = SamplerOption::TraceIdRatioBased(0.5);

        let recorder = RecordingProcessor::default();
        let processor = make_processor(recorder.clone(), per_exporter, false, global);

        // Same trace ID that would be a parent-flag override when parent_based_sampler=true.
        let span = make_span(u64::MAX as u128);
        processor.on_end(span);

        assert_eq!(
            recorder.0.lock().unwrap().len(),
            0,
            "with parent_based_sampler disabled, no override path exists — span must be dropped"
        );
    }
}
