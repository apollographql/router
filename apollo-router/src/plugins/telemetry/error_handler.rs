use std::fmt::Debug;
use std::sync::Weak;
use std::time::Duration;

use opentelemetry::Value;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::error::OTelSdkError;
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
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;

use crate::metrics::meter_provider;

const OTEL_METRIC_OVERFLOW_KEY: &str = "otel.metric.overflow";

/// Wrapper that modifies trace export errors to include exporter name
pub(crate) struct NamedSpanExporter<E> {
    name: &'static str,
    inner: E,
}

impl<E> NamedSpanExporter<E> {
    pub(crate) fn new(inner: E, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<E: SpanExporter> Debug for NamedSpanExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedSpanExporter")
            .field("name", &self.name)
            .finish()
    }
}

impl<E: SpanExporter> SpanExporter for NamedSpanExporter<E> {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(batch);
        async move {
            fut.await
                .map_err(|err| OTelSdkError::InternalFailure(format!("[{} traces] {}", name, err)))
        }
    }

    fn shutdown(&mut self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&mut self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.inner.set_resource(resource)
    }
}

/// Wrapper for metric exporters and readers that:
/// - Detects cardinality overflow and increments the overflow counter
/// - For push exporters: prefixes error messages with the exporter name
///
/// Implements `PushMetricExporter` when `T: PushMetricExporter` and
/// `MetricReader` when `T: MetricReader`.
pub(crate) struct NamedMetricExporter<T> {
    /// Name used for error prefixing (only used for PushMetricExporter)
    name: &'static str,
    inner: T,
}

impl<T: Clone> Clone for NamedMetricExporter<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            inner: self.inner.clone(),
        }
    }
}

impl<T> NamedMetricExporter<T> {
    pub(crate) fn new_push(inner: T, name: &'static str) -> Self {
        Self { name, inner }
    }

    pub(crate) fn new_pull(inner: T, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<T: Debug> Debug for NamedMetricExporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedMetricExporter")
            .field("name", &self.name)
            .field("inner", &self.inner)
            .finish()
    }
}

fn prefix_otel_error(name: &'static str, err: OTelSdkError) -> OTelSdkError {
    match err {
        OTelSdkError::AlreadyShutdown => OTelSdkError::AlreadyShutdown,
        OTelSdkError::Timeout(d) => OTelSdkError::Timeout(d),
        OTelSdkError::InternalFailure(msg) => {
            OTelSdkError::InternalFailure(format!("[{} metrics] {}", name, msg))
        }
    }
}

/// Implementation for push-based exporters (OTLP, Apollo, etc.)
impl<T: PushMetricExporter> PushMetricExporter for NamedMetricExporter<T> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        report_cardinality_overflow(metrics);
        let name = self.name;
        let fut = self.inner.export(metrics);
        async move { fut.await.map_err(|err| prefix_otel_error(name, err)) }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner
            .force_flush()
            .map_err(|err| prefix_otel_error(self.name, err))
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner
            .shutdown_with_timeout(timeout)
            .map_err(|err| prefix_otel_error(self.name, err))
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

impl<T> NamedMetricExporter<T> {
    /// Backward-compatible constructor for push exporters
    #[allow(dead_code)]
    pub(crate) fn new(inner: T, name: &'static str) -> Self {
        Self::new_push(inner, name)
    }
}

/// Implementation for pull-based readers (Prometheus)
impl<T: MetricReader> MetricReader for NamedMetricExporter<T> {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.inner.register_pipeline(pipeline)
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        let result = self.inner.collect(rm);
        if result.is_ok() {
            report_cardinality_overflow(rm);
        }
        result
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

const CARDINALITY_OVERFLOW_METRIC: &str = "apollo.router.telemetry.metrics.cardinality_overflow";

/// Check for cardinality overflow in metrics and report via counter.
///
/// When OpenTelemetry SDK exceeds cardinality limits for a metric, it aggregates
/// overflow measurements into a special data point marked with `otel.metric.overflow=true`.
/// This function detects those overflow data points and increments a counter to make
/// the overflow visible to monitoring systems.
fn report_cardinality_overflow(metrics: &ResourceMetrics) {
    for scope_metrics in metrics.scope_metrics() {
        for metric in scope_metrics.metrics() {
            // Skip our own overflow counter to avoid recursion
            if metric.name() == CARDINALITY_OVERFLOW_METRIC {
                continue;
            }
            if has_overflow_data_point(metric) {
                let meter = meter_provider().meter("apollo/router");
                let counter = meter
                    .u64_counter(CARDINALITY_OVERFLOW_METRIC)
                    .with_description("Counts metrics that have exceeded their cardinality limit")
                    .build();
                counter.add(
                    1,
                    &[opentelemetry::KeyValue::new(
                        "metric.name",
                        metric.name().to_string(),
                    )],
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
    use std::fmt::Debug;
    use std::time::Duration;

    use opentelemetry::KeyValue;
    use opentelemetry::Value;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::metrics::Stream;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;

    use crate::metrics::FutureMetricsExt;
    use crate::metrics::test_utils::ClonableManualReader;
    use crate::plugins::telemetry::error_handler::NamedMetricExporter;
    use crate::plugins::telemetry::error_handler::OTEL_METRIC_OVERFLOW_KEY;
    use crate::plugins::telemetry::error_handler::has_overflow_attribute;

    // Mock span exporter to test failures
    #[derive(Debug)]
    struct FailingSpanExporter;

    impl SpanExporter for FailingSpanExporter {
        async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
            Err(OTelSdkError::InternalFailure(
                "connection failed".to_string(),
            ))
        }

        fn shutdown(&mut self) -> OTelSdkResult {
            Ok(())
        }

        fn force_flush(&mut self) -> OTelSdkResult {
            Ok(())
        }

        fn set_resource(&mut self, _resource: &opentelemetry_sdk::Resource) {}
    }

    #[tokio::test]
    async fn test_named_span_exporter_adds_prefix() {
        let inner = FailingSpanExporter;
        let named = super::NamedSpanExporter::new(inner, "test-exporter");

        let result = named.export(vec![]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("[test-exporter traces]"));
        assert!(err_msg.contains("connection failed"));
    }

    // Mock metrics exporter to test failures
    #[derive(Debug)]
    struct FailingMetricExporter;

    impl PushMetricExporter for FailingMetricExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            Err(OTelSdkError::InternalFailure("export failed".to_string()))
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    fn empty_resource_metrics() -> ResourceMetrics {
        ResourceMetrics::default()
    }

    #[tokio::test]
    async fn test_named_metric_exporter_adds_prefix() {
        let inner = FailingMetricExporter;
        let named = super::NamedMetricExporter::new(inner, "test-exporter");

        let result = named.export(&empty_resource_metrics()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            OTelSdkError::InternalFailure(msg) => {
                assert!(msg.contains("[test-exporter metrics]"));
                assert!(msg.contains("export failed"));
            }
            _ => panic!("Expected InternalFailure, got: {:?}", err),
        }
    }

    #[test]
    fn test_prefix_otel_error() {
        let err = OTelSdkError::InternalFailure("bad config".to_string());
        let prefixed = super::prefix_otel_error("test-exporter", err);

        match prefixed {
            OTelSdkError::InternalFailure(msg) => {
                assert_eq!(msg, "[test-exporter metrics] bad config");
            }
            _ => panic!("Expected InternalFailure variant"),
        }
    }

    #[test]
    fn detects_overflow_attribute() {
        let attrs = vec![
            KeyValue::new("http.method", "GET"),
            KeyValue::new(OTEL_METRIC_OVERFLOW_KEY, true),
        ];
        assert!(has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_missing() {
        let attrs = vec![
            KeyValue::new("http.method", "GET"),
            KeyValue::new("http.status_code", 200),
        ];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_is_false() {
        let attrs = vec![KeyValue::new(OTEL_METRIC_OVERFLOW_KEY, false)];
        assert!(!has_overflow_attribute(attrs.iter()));
    }

    #[test]
    fn no_overflow_when_attribute_is_wrong_type() {
        let attrs = vec![KeyValue::new(
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

            // Export through NamedMetricExporter which should detect overflow and increment counter
            let inner_exporter = InMemoryMetricExporter::default();
            let exporter = NamedMetricExporter::new(inner_exporter, "test");
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

    #[tokio::test]
    async fn pull_reader_increments_counter_on_overflow() {
        use crate::plugins::telemetry::error_handler::NamedMetricExporter;

        async {
            // Create a cloneable reader wrapped with overflow detection (simulates Prometheus path)
            let inner_reader = ClonableManualReader::default();
            let reader = NamedMetricExporter::new_pull(inner_reader, "prometheus");

            // Clone the reader before passing to builder so we can call collect() later
            let reader_for_collect = reader.clone();

            let provider = SdkMeterProvider::builder()
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
                .build();

            // Record metrics that exceed cardinality limit
            let meter = provider.meter("test");
            let counter = meter.u64_counter("test.pull.overflow.metric").build();
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value1")]);
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value2")]);
            counter.add(1, &[opentelemetry::KeyValue::new("key", "value3")]); // Overflow

            // Collect via the wrapped reader (simulates Prometheus scrape triggering overflow detection)
            let mut resource_metrics = ResourceMetrics::default();
            reader_for_collect.collect(&mut resource_metrics).unwrap();

            // Verify the overflow counter was incremented
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.pull.overflow.metric"
            );
        }
        .with_metrics()
        .await
    }
}
