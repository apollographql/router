//! Cardinality overflow detection for metric exporters.
//!
//! When OpenTelemetry SDK exceeds cardinality limits for a metric, it aggregates
//! overflow measurements into a special data point marked with `otel.metric.overflow=true`.
//! This module provides wrappers that detect those overflow data points and increment
//! a counter to make the overflow visible to monitoring systems.
//!
//! Push exporters (OTLP, Apollo) are checked on every export. The Prometheus exporter serves
//! scrapes from its own internal collector, which never calls back into a reader wrapper, so the
//! Prometheus endpoint passes each scrape to [`OverflowMetricReader`] to check.
//!
//! Each meter provider must count an overflow once. The metrics builder decides which wrapper
//! counts for the public meter provider; see [`OverflowCounting`].

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use opentelemetry::Value;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::Pipeline;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::Metric;
use opentelemetry_sdk::metrics::data::MetricData;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader;
use parking_lot::Mutex;
use prometheus::proto::MetricFamily;

const OTEL_METRIC_OVERFLOW_KEY: &str = "otel.metric.overflow";
/// [`OTEL_METRIC_OVERFLOW_KEY`] as the Prometheus exporter writes it, with dots sanitized.
const PROMETHEUS_OVERFLOW_LABEL: &str = "otel_metric_overflow";
const CARDINALITY_OVERFLOW_METRIC: &str = "apollo.router.telemetry.metrics.cardinality_overflow";

/// Whether an [`OverflowMetricExporter`] counts the overflow it sees.
///
/// Shared with the metrics builder, which enables it once every exporter is configured, so it
/// can choose a single counting source for a meter provider. Disabled by default.
#[derive(Clone, Debug, Default)]
pub(crate) struct OverflowCounting(Arc<AtomicBool>);

impl OverflowCounting {
    pub(crate) fn enabled() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    pub(crate) fn enable(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_enabled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Wrapper for push metric exporters that detects cardinality overflow.
pub(crate) struct OverflowMetricExporter<T> {
    inner: T,
    counting: OverflowCounting,
}

impl<T: Clone> Clone for OverflowMetricExporter<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            counting: self.counting.clone(),
        }
    }
}

impl<T> OverflowMetricExporter<T> {
    /// Create a new overflow-detecting wrapper for push-based exporters that always counts.
    pub(crate) fn new_push(inner: T) -> Self {
        Self::with_counting(inner, OverflowCounting::enabled())
    }

    /// Create a wrapper that counts only while `counting` is enabled.
    pub(crate) fn with_counting(inner: T, counting: OverflowCounting) -> Self {
        Self { inner, counting }
    }

    #[cfg(test)]
    pub(crate) fn counts_overflow(&self) -> bool {
        self.counting.is_enabled()
    }
}

impl<T: Debug> Debug for OverflowMetricExporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverflowMetricExporter")
            .field("inner", &self.inner)
            .field("counting", &self.counting.is_enabled())
            .finish()
    }
}

/// Implementation for push-based exporters (OTLP, Apollo, etc.)
impl<T: PushMetricExporter> PushMetricExporter for OverflowMetricExporter<T> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        if self.counting.is_enabled() {
            overflowing_metric_names(metrics).for_each(record_overflow);
        }
        self.inner.export(metrics)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

/// Wrapper for pull readers (Prometheus) that checks each scrape for cardinality overflow.
///
/// The SDK owns the registered reader, so the wrapper shares it: one clone is registered with the
/// meter provider and another is kept by whoever serves the pull endpoint.
pub(crate) struct OverflowMetricReader<T> {
    inner: Arc<T>,
    overflowing: Arc<Mutex<OverflowingMetrics>>,
}

/// The overflowing metrics found by the last collect, keyed by the scraped families that showed
/// overflow at the time.
#[derive(Debug, Default)]
struct OverflowingMetrics {
    families: Vec<String>,
    metric_names: Vec<String>,
}

impl<T> Clone for OverflowMetricReader<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            overflowing: self.overflowing.clone(),
        }
    }
}

impl<T: Debug> Debug for OverflowMetricReader<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverflowMetricReader")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T: MetricReader> OverflowMetricReader<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner: Arc::new(inner),
            overflowing: Default::default(),
        }
    }

    /// Increment the overflow counter for every metric that has overflowed in a scrape.
    ///
    /// Scraped families carry Prometheus names, but the counter reports OpenTelemetry instrument
    /// names, as the push exporters do. Those are read by collecting from the wrapped reader, which
    /// with cumulative temporality doesn't reset the aggregation state. The result is reused
    /// until the set of overflowing families changes, so a persistent overflow doesn't collect
    /// twice on every scrape.
    pub(crate) fn report_cardinality_overflow(&self, scrape: &[MetricFamily]) {
        let families: Vec<&str> = scrape
            .iter()
            .filter(|family| family_has_overflow(family))
            .map(MetricFamily::name)
            .collect();
        if families.is_empty() {
            return;
        }

        let mut overflowing = self.overflowing.lock();
        if !overflowing.families.iter().eq(families.iter()) {
            let mut rm = ResourceMetrics::default();
            if let Err(err) = self.inner.collect(&mut rm) {
                tracing::debug!("could not collect metrics to check cardinality overflow: {err}");
                return;
            }
            *overflowing = OverflowingMetrics {
                families: families.iter().map(ToString::to_string).collect(),
                metric_names: overflowing_metric_names(&rm)
                    .map(ToString::to_string)
                    .collect(),
            };
        }
        overflowing
            .metric_names
            .iter()
            .map(String::as_str)
            .for_each(record_overflow);
    }
}

impl<T: MetricReader> MetricReader for OverflowMetricReader<T> {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.inner.register_pipeline(pipeline)
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.inner.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.inner.temporality(kind)
    }
}

/// Names of the metrics that have overflow data points, excluding our own overflow counter to
/// avoid recursion.
fn overflowing_metric_names(metrics: &ResourceMetrics) -> impl Iterator<Item = &str> {
    metrics
        .scope_metrics()
        .flat_map(|scope_metrics| scope_metrics.metrics())
        .filter(|metric| {
            metric.name() != CARDINALITY_OVERFLOW_METRIC && has_overflow_data_point(metric)
        })
        .map(Metric::name)
}

fn record_overflow(metric_name: &str) {
    u64_counter_with_unit!(
        "apollo.router.telemetry.metrics.cardinality_overflow",
        "Counts metrics that have exceeded their cardinality limit",
        "count",
        1,
        [opentelemetry::KeyValue::new(
            "metric.name",
            metric_name.to_string(),
        )]
    );
}

/// Check if a metric has any data points with the overflow attribute.
fn has_overflow_data_point(metric: &Metric) -> bool {
    match metric.data() {
        AggregatedMetrics::F64(data) => has_overflow_in_metric_data(data),
        AggregatedMetrics::U64(data) => has_overflow_in_metric_data(data),
        AggregatedMetrics::I64(data) => has_overflow_in_metric_data(data),
    }
}

/// Check if any data point in a MetricData has the overflow attribute.
fn has_overflow_in_metric_data<T>(data: &MetricData<T>) -> bool {
    match data {
        MetricData::Gauge(gauge) => gauge
            .data_points()
            .any(|dp| has_overflow_attribute(dp.attributes())),
        MetricData::Sum(sum) => sum
            .data_points()
            .any(|dp| has_overflow_attribute(dp.attributes())),
        MetricData::Histogram(hist) => hist
            .data_points()
            .any(|dp| has_overflow_attribute(dp.attributes())),
        MetricData::ExponentialHistogram(exp_hist) => exp_hist
            .data_points()
            .any(|dp| has_overflow_attribute(dp.attributes())),
    }
}

/// Check if attributes contain the overflow marker.
fn has_overflow_attribute<'a>(attrs: impl Iterator<Item = &'a opentelemetry::KeyValue>) -> bool {
    attrs
        .into_iter()
        .any(|kv| kv.key.as_str() == OTEL_METRIC_OVERFLOW_KEY && kv.value == Value::Bool(true))
}

/// Check if any series in a scraped Prometheus family carries the overflow marker.
fn family_has_overflow(family: &MetricFamily) -> bool {
    family.get_metric().iter().any(|metric| {
        metric
            .get_label()
            .iter()
            .any(|label| label.name() == PROMETHEUS_OVERFLOW_LABEL && label.value() == "true")
    })
}

#[cfg(test)]
mod tests {
    use opentelemetry::KeyValue;
    use opentelemetry::Value;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::metrics::Stream;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use prometheus::proto::LabelPair;
    use prometheus::proto::Metric as PrometheusMetric;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::metrics::test_utils::ClonableManualReader;

    #[test]
    fn detects_overflow_attribute() {
        let attrs = [
            KeyValue::new("http.method", "GET"),
            KeyValue::new(OTEL_METRIC_OVERFLOW_KEY, true),
        ];
        assert!(has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_missing() {
        let attrs = [
            KeyValue::new("http.method", "GET"),
            KeyValue::new("http.status_code", 200),
        ];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_is_false() {
        let attrs = [KeyValue::new(OTEL_METRIC_OVERFLOW_KEY, false)];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_is_wrong_type() {
        let attrs = [KeyValue::new(
            OTEL_METRIC_OVERFLOW_KEY,
            Value::String("true".into()),
        )];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_on_empty_attributes() {
        let attrs: Vec<KeyValue> = vec![];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[tokio::test]
    async fn increments_counter_on_cardinality_overflow() {
        async {
            // Create a meter provider with a very low cardinality limit for our test metric
            let reader = ClonableManualReader::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(reader.clone())
                .with_resource(Resource::builder_empty().build())
                .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                    if instrument.name() == "test.overflow.metric" {
                        Some(
                            Stream::builder()
                                .with_cardinality_limit(2) // Very low limit to trigger overflow
                                .build()
                                .expect("valid stream"),
                        )
                    } else {
                        None
                    }
                })
                .build();

            // Record metrics that will exceed the cardinality limit
            let meter = provider.meter("test");
            let counter = meter.u64_counter("test.overflow.metric").build();

            // Record with 3 different attribute sets to exceed limit of 2
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value1")]);
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value2")]);
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value3")]); // This should overflow

            // Collect metrics from the test provider
            let mut resource_metrics = ResourceMetrics::default();
            reader.collect(&mut resource_metrics).unwrap();

            // Export through OverflowMetricExporter which should detect overflow and increment counter
            let inner_exporter = InMemoryMetricExporter::default();
            let exporter = OverflowMetricExporter::new_push(inner_exporter);
            exporter.export(&resource_metrics).await.unwrap();

            // Verify the overflow counter was incremented
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.overflow.metric"
            );
        }
        .with_metrics()
        .await
    }

    #[test]
    fn prometheus_label_is_the_sanitized_overflow_key() {
        assert_eq!(
            OTEL_METRIC_OVERFLOW_KEY.replace('.', "_"),
            PROMETHEUS_OVERFLOW_LABEL
        );
    }

    fn overflowing_provider(
        reader: OverflowMetricReader<ClonableManualReader>,
    ) -> SdkMeterProvider {
        SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(Resource::builder_empty().build())
            .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                instrument.name().starts_with("test.pull.").then(|| {
                    Stream::builder()
                        .with_cardinality_limit(2)
                        .build()
                        .expect("valid stream")
                })
            })
            .build()
    }

    /// Records three attribute sets, one past the provider's limit of two.
    fn overflow(provider: &SdkMeterProvider, name: &'static str) {
        let counter = provider.meter("test").u64_counter(name).build();
        for value in ["value1", "value2", "value3"] {
            counter.add(1, &[KeyValue::new("key", value)]);
        }
    }

    /// A scraped Prometheus family, with the overflow marker on one series if `overflowed`.
    fn scraped_family(name: &str, overflowed: bool) -> MetricFamily {
        let label = |name: &str, value: &str| {
            let mut label = LabelPair::new();
            label.set_name(name.to_string());
            label.set_value(value.to_string());
            label
        };
        let mut series = PrometheusMetric::new();
        series.set_label(vec![label("key", "value1")]);
        let mut overflow_series = PrometheusMetric::new();
        overflow_series.set_label(vec![label(
            PROMETHEUS_OVERFLOW_LABEL,
            if overflowed { "true" } else { "false" },
        )]);
        let mut family = MetricFamily::new();
        family.set_name(name.to_string());
        family.set_metric(vec![series, overflow_series]);
        family
    }

    #[tokio::test]
    async fn pull_reader_reports_overflow_with_instrument_name() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = overflowing_provider(reader.clone());
            overflow(&provider, "test.pull.overflow.metric");
            let scrape = [scraped_family("test_pull_overflow_metric_total", true)];

            reader.report_cardinality_overflow(&scrape);
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.pull.overflow.metric"
            );

            // The overflow persists in cumulative state, so every scrape reports it.
            reader.report_cardinality_overflow(&scrape);
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                2,
                "metric.name" = "test.pull.overflow.metric"
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn pull_reader_collects_again_only_when_overflowing_families_change() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = overflowing_provider(reader.clone());
            overflow(&provider, "test.pull.first");
            let first = scraped_family("test_pull_first_total", true);

            reader.report_cardinality_overflow(std::slice::from_ref(&first));

            // A second metric overflows, but the scrape shows the same overflowing families, so
            // the cached result is reused without collecting.
            overflow(&provider, "test.pull.second");
            reader.report_cardinality_overflow(std::slice::from_ref(&first));
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                2,
                "metric.name" = "test.pull.first"
            );
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = "test.pull.second"
            );

            // Once the scrape shows the new family, the reader collects again.
            reader.report_cardinality_overflow(&[
                first,
                scraped_family("test_pull_second_total", true),
            ]);
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                3,
                "metric.name" = "test.pull.first"
            );
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.pull.second"
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn pull_reader_does_not_report_without_overflow() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = overflowing_provider(reader.clone());
            // The scrape decides whether to look: an overflow it doesn't show isn't reported.
            overflow(&provider, "test.pull.overflow.metric");

            reader.report_cardinality_overflow(&[scraped_family(
                "test_pull_overflow_metric_total",
                false,
            )]);
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = "test.pull.overflow.metric"
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn does_not_count_its_own_overflow() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = SdkMeterProvider::builder()
                .with_reader(reader.clone())
                .with_resource(Resource::builder_empty().build())
                .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                    (instrument.name() == CARDINALITY_OVERFLOW_METRIC).then(|| {
                        Stream::builder()
                            .with_cardinality_limit(1)
                            .build()
                            .expect("valid stream")
                    })
                })
                .build();

            // The overflow counter itself overflows in this provider.
            let counter = provider
                .meter("test")
                .u64_counter(CARDINALITY_OVERFLOW_METRIC)
                .build();
            counter.add(1, &[KeyValue::new("metric.name", "a")]);
            counter.add(1, &[KeyValue::new("metric.name", "b")]);

            reader.report_cardinality_overflow(&[scraped_family(
                "apollo_router_telemetry_metrics_cardinality_overflow_total",
                true,
            )]);
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = CARDINALITY_OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn push_exporter_counts_only_when_enabled() {
        async {
            let reader = ClonableManualReader::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(reader.clone())
                .with_resource(Resource::builder_empty().build())
                .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                    (instrument.name() == "test.push.overflow.metric").then(|| {
                        Stream::builder()
                            .with_cardinality_limit(2)
                            .build()
                            .expect("valid stream")
                    })
                })
                .build();
            overflow(&provider, "test.push.overflow.metric");
            let mut resource_metrics = ResourceMetrics::default();
            reader.collect(&mut resource_metrics).unwrap();

            let counting = OverflowCounting::default();
            let exporter = OverflowMetricExporter::with_counting(
                InMemoryMetricExporter::default(),
                counting.clone(),
            );
            exporter.export(&resource_metrics).await.unwrap();
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = "test.push.overflow.metric"
            );

            counting.enable();
            exporter.export(&resource_metrics).await.unwrap();
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.push.overflow.metric"
            );
        }
        .with_metrics()
        .await
    }
}
