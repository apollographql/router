//! Cardinality overflow detection for metric exporters.
//!
//! When OpenTelemetry SDK exceeds cardinality limits for a metric, it aggregates
//! overflow measurements into a special data point marked with `otel.metric.overflow=true`.
//! This module provides wrappers that detect those overflow data points and increment
//! a counter to make the overflow visible to monitoring systems.
//!
//! Push exporters (OTLP, Apollo) are checked on every export. The Prometheus exporter serves
//! scrapes from its own internal collector, which never calls back into a reader wrapper, so the
//! Prometheus endpoint asks [`OverflowMetricReader`] to check when a scrape contains overflow.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Weak;
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

const OTEL_METRIC_OVERFLOW_KEY: &str = "otel.metric.overflow";
const CARDINALITY_OVERFLOW_METRIC: &str = "apollo.router.telemetry.metrics.cardinality_overflow";

/// Wrapper for push metric exporters that detects cardinality overflow.
pub(crate) struct OverflowMetricExporter<T> {
    inner: T,
}

impl<T: Clone> Clone for OverflowMetricExporter<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> OverflowMetricExporter<T> {
    /// Create a new overflow-detecting wrapper for push-based exporters.
    pub(crate) fn new_push(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: Debug> Debug for OverflowMetricExporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverflowMetricExporter")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Implementation for push-based exporters (OTLP, Apollo, etc.)
impl<T: PushMetricExporter> PushMetricExporter for OverflowMetricExporter<T> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        report_cardinality_overflow(metrics);
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

/// Wrapper for pull readers (Prometheus) that can check for cardinality overflow on demand.
///
/// The SDK owns the registered reader, so the wrapper shares it: one clone is registered with the
/// meter provider and another is kept by whoever serves the pull endpoint.
pub(crate) struct OverflowMetricReader<T> {
    inner: Arc<T>,
}

impl<T> Clone for OverflowMetricReader<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
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
        }
    }

    /// Collect from the wrapped reader and increment the overflow counter for every metric that
    /// has an overflow data point.
    ///
    /// The collected data carries OpenTelemetry instrument names, so `metric.name` matches what the
    /// push exporters report. With cumulative temporality this reads the aggregation state without
    /// resetting it; observable instrument callbacks run once more per call.
    pub(crate) fn report_cardinality_overflow(&self) {
        let mut rm = ResourceMetrics::default();
        match self.inner.collect(&mut rm) {
            Ok(()) => report_cardinality_overflow(&rm),
            Err(err) => {
                tracing::debug!("could not collect metrics to check cardinality overflow: {err}")
            }
        }
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

/// Check for cardinality overflow in metrics and report via counter.
fn report_cardinality_overflow(metrics: &ResourceMetrics) {
    for scope_metrics in metrics.scope_metrics() {
        for metric in scope_metrics.metrics() {
            // Skip our own overflow counter to avoid recursion
            if metric.name() == CARDINALITY_OVERFLOW_METRIC {
                continue;
            }
            if has_overflow_data_point(metric) {
                u64_counter_with_unit!(
                    "apollo.router.telemetry.metrics.cardinality_overflow",
                    "Counts metrics that have exceeded their cardinality limit",
                    "count",
                    1,
                    [opentelemetry::KeyValue::new(
                        "metric.name",
                        metric.name().to_string(),
                    )]
                );
            }
        }
    }
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

    fn overflowing_provider(
        reader: OverflowMetricReader<ClonableManualReader>,
    ) -> SdkMeterProvider {
        SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(Resource::builder_empty().build())
            .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                if instrument.name() == "test.pull.overflow.metric" {
                    Some(
                        Stream::builder()
                            .with_cardinality_limit(2)
                            .build()
                            .expect("valid stream"),
                    )
                } else {
                    None
                }
            })
            .build()
    }

    #[tokio::test]
    async fn pull_reader_reports_overflow_with_instrument_name() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = overflowing_provider(reader.clone());

            let meter = provider.meter("test");
            let counter = meter.u64_counter("test.pull.overflow.metric").build();
            counter.add(1, &[KeyValue::new("key", "value1")]);
            counter.add(1, &[KeyValue::new("key", "value2")]);
            counter.add(1, &[KeyValue::new("key", "value3")]); // Overflow

            reader.report_cardinality_overflow();
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.pull.overflow.metric"
            );

            // Collecting again does not reset cumulative state: the overflow is still reported.
            reader.report_cardinality_overflow();
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
    async fn pull_reader_does_not_report_without_overflow() {
        async {
            let reader = OverflowMetricReader::new(ClonableManualReader::default());
            let provider = overflowing_provider(reader.clone());

            let meter = provider.meter("test");
            let counter = meter.u64_counter("test.pull.overflow.metric").build();
            counter.add(1, &[KeyValue::new("key", "value1")]);
            counter.add(1, &[KeyValue::new("key", "value2")]);

            reader.report_cardinality_overflow();
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

            reader.report_cardinality_overflow();
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = CARDINALITY_OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }
}
