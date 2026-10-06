//! Cardinality overflow detection for metric exporters.
//!
//! When OpenTelemetry SDK exceeds cardinality limits for a metric, it aggregates
//! overflow measurements into a special data point marked with `otel.metric.overflow=true`.
//! This module detects those overflow data points and increments
//! `apollo.router.telemetry.metrics.cardinality_overflow`, with the OpenTelemetry name of the
//! overflowing metric as its `metric.name` attribute.
//!
//! For the public meter provider the counter goes up when a metric starts overflowing, not on
//! every collection; see [`OverflowTracker`]. Push exporters (OTLP, Apollo usage reporting) are
//! checked on every export. The Prometheus exporter is a pull exporter: it serves scrapes from its
//! own internal collector, which never calls back into a reader wrapper, so the Prometheus
//! endpoint checks each scrape itself. The Apollo usage-reporting exporters don't track starts:
//! they count every export that has overflow, as they always have.
//!
//! Each meter provider must count an overflow once. The metrics builder decides which source
//! counts for the public meter provider; see [`OverflowCounting`].

use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use opentelemetry::Value;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::Metric;
use opentelemetry_sdk::metrics::data::MetricData;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use parking_lot::Mutex;
use prometheus::proto::MetricFamily;

const OTEL_METRIC_OVERFLOW_KEY: &str = "otel.metric.overflow";
/// [`OTEL_METRIC_OVERFLOW_KEY`] for the Prometheus exporter.
const PROMETHEUS_OVERFLOW_LABEL: &str = "otel_metric_overflow";
const CARDINALITY_OVERFLOW_METRIC: &str = "apollo.router.telemetry.metrics.cardinality_overflow";
/// [`CARDINALITY_OVERFLOW_METRIC`] for the Prometheus exporter.
const PROMETHEUS_CARDINALITY_OVERFLOW_FAMILY: &str =
    "apollo_router_telemetry_metrics_cardinality_overflow_total";

/// How an [`OverflowMetricExporter`] counts the overflow it sees.
#[derive(Clone, Debug)]
pub(crate) enum OverflowCounting {
    /// Count every export that has overflow. Used by the Apollo meter providers, which each have
    /// a single exporter.
    EveryExport,
    /// Count when a metric starts overflowing. Used by the public meter provider's counting
    /// source.
    EveryOverflowStart(OverflowTracker),
    /// Don't count, because another source on the same meter provider does.
    Off,
}

/// Metrics observed overflowing during the last collection by a counting source.
///
/// The counter goes up once when a metric starts overflowing, not again while it stays
/// overflowed. Cumulative sums and histograms keep an overflow until their pipeline is rebuilt,
/// so they count once. Observable gauges reflect each collection, and a delta exporter starts
/// each interval empty, so a metric can stop overflowing and later start again; each observed
/// restart counts again.
#[derive(Clone, Debug, Default)]
pub(crate) struct OverflowTracker(Arc<Mutex<Overflowing>>);

#[derive(Debug, Default)]
struct Overflowing {
    /// OpenTelemetry names of the metrics overflowing in the last collection.
    metrics: HashSet<String>,
    /// Prometheus names of the families overflowing in the last scrape.
    families: HashSet<String>,
}

impl OverflowTracker {
    /// Record the metrics overflowing in one export, counting those that weren't overflowing in
    /// the previous one.
    fn record<'a>(&self, overflowing: impl IntoIterator<Item = &'a str>) {
        let started = track_starts(&mut self.0.lock().metrics, overflowing);
        started
            .iter()
            .map(String::as_str)
            .for_each(record_cardinality_overflow);
    }

    /// Gather a Prometheus scrape and record the metrics that started overflowing since the
    /// previous scrape.
    ///
    /// A scrape only has Prometheus family names, and the exporter's name conversion can map
    /// different OpenTelemetry names to one family. So when the set of overflowing families
    /// changes, `collect` reads the same pipeline once more, as the Prometheus exporter's own
    /// reader, for the OpenTelemetry names of the overflowing metrics. Scrapes that show no change
    /// do no extra work.
    ///
    /// The scrape and the collect are separate readings. An observable gauge runs its callback for
    /// each, so it can overflow in one and not the other. If the collect finds fewer overflowing
    /// metrics than the scrape found families, the family set isn't saved, so the next scrape
    /// collects again rather than treating those families as already counted. Two metrics that
    /// share a family are still missed in one case: if the second starts overflowing while the
    /// first already is, it counts only once the family set next changes.
    ///
    /// The lock is held across `gather` and `collect` so that concurrent scrapes are compared in
    /// the order they were taken. Otherwise an older snapshot recorded after a newer one would
    /// end an overflow that never cleared, and the next scrape would count it again.
    pub(crate) fn gather_and_record(
        &self,
        gather: impl FnOnce() -> Vec<MetricFamily>,
        collect: impl FnOnce(&mut ResourceMetrics) -> OTelSdkResult,
    ) -> Vec<MetricFamily> {
        let (scrape, started) = self.gather_and_track(gather, collect);
        started
            .iter()
            .map(String::as_str)
            .for_each(record_cardinality_overflow);
        scrape
    }

    fn gather_and_track(
        &self,
        gather: impl FnOnce() -> Vec<MetricFamily>,
        collect: impl FnOnce(&mut ResourceMetrics) -> OTelSdkResult,
    ) -> (Vec<MetricFamily>, Vec<String>) {
        let mut overflowing = self.0.lock();
        let scrape = gather();
        let families: HashSet<String> = overflowing_prometheus_names(&scrape)
            .map(str::to_string)
            .collect();
        if families == overflowing.families {
            return (scrape, Vec::new());
        }
        let mut metrics = ResourceMetrics::default();
        if let Err(error) = collect(&mut metrics) {
            // Leave the families unchanged so that the next scrape tries again.
            tracing::debug!(%error, "could not collect metrics to name overflowing families");
            return (scrape, Vec::new());
        }
        let named: HashSet<&str> = overflowing_otel_names(&metrics).collect();
        // Only save the families once the collect has named at least as many metrics; otherwise
        // the next scrape would see an unchanged set and never collect for the ones missing here.
        if named.len() >= families.len() {
            overflowing.families = families;
        }
        let started = track_starts(&mut overflowing.metrics, named);
        (scrape, started)
    }
}

/// Replace the previous overflowing set with the current one, returning the names that started.
fn track_starts<'a>(
    previous: &mut HashSet<String>,
    overflowing: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let overflowing: HashSet<String> = overflowing.into_iter().map(str::to_string).collect();
    let started = overflowing.difference(previous).cloned().collect();
    *previous = overflowing;
    started
}

/// Wrapper for push metric exporters that detects cardinality overflow.
///
/// Exporters on the public meter provider are created through
/// `MetricsBuilder::public_overflow_exporter`, which chooses their [`OverflowCounting`] so that
/// only one source counts.
#[derive(Clone, Debug)]
pub(crate) struct OverflowMetricExporter<T> {
    inner: T,
    counting: OverflowCounting,
}

impl<T> OverflowMetricExporter<T> {
    pub(crate) fn new(inner: T, counting: OverflowCounting) -> Self {
        Self { inner, counting }
    }

    #[cfg(test)]
    pub(crate) fn is_counting(&self) -> bool {
        !matches!(self.counting, OverflowCounting::Off)
    }
}

/// Implementation for push-based exporters (OTLP, Apollo, etc.)
impl<T: PushMetricExporter> PushMetricExporter for OverflowMetricExporter<T> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        match &self.counting {
            OverflowCounting::EveryExport => {
                overflowing_otel_names(metrics).for_each(record_cardinality_overflow)
            }
            OverflowCounting::EveryOverflowStart(tracker) => {
                tracker.record(overflowing_otel_names(metrics))
            }
            OverflowCounting::Off => {}
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

/// Names of the metrics that have overflow data points, except our own counter's.
fn overflowing_otel_names(metrics: &ResourceMetrics) -> impl Iterator<Item = &str> {
    metrics
        .scope_metrics()
        .flat_map(|scope_metrics| scope_metrics.metrics())
        .filter(|metric| {
            metric.name() != CARDINALITY_OVERFLOW_METRIC && has_overflow_data_point(metric)
        })
        .map(Metric::name)
}

/// Names of the scraped Prometheus families that have overflow series, except our own counter's.
fn overflowing_prometheus_names(scrape: &[MetricFamily]) -> impl Iterator<Item = &str> {
    scrape
        .iter()
        .filter(|family| {
            family.name() != PROMETHEUS_CARDINALITY_OVERFLOW_FAMILY && family_has_overflow(family)
        })
        .map(MetricFamily::name)
}

fn record_cardinality_overflow(metric_name: &str) {
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
    use crate::plugins::telemetry::metrics::prometheus::SharedPrometheusExporter;

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

    const OVERFLOW_METRIC: &str = "test.overflow.metric";
    const OVERFLOW_FAMILY: &str = "test_overflow_metric_total";

    fn limited_provider(reader: ClonableManualReader) -> SdkMeterProvider {
        SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(Resource::builder_empty().build())
            .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                instrument.name().starts_with("test.").then(|| {
                    Stream::builder()
                        .with_cardinality_limit(2)
                        .build()
                        .expect("valid stream")
                })
            })
            .build()
    }

    /// A collection of [`OVERFLOW_METRIC`] with `values` distinct attribute sets, past the limit
    /// of two when `values` is three or more.
    fn collected(values: usize) -> ResourceMetrics {
        let reader = ClonableManualReader::default();
        let provider = limited_provider(reader.clone());
        let counter = provider.meter("test").u64_counter(OVERFLOW_METRIC).build();
        for value in 0..values {
            counter.add(1, &[KeyValue::new("key", value as i64)]);
        }
        let mut resource_metrics = ResourceMetrics::default();
        reader.collect(&mut resource_metrics).unwrap();
        resource_metrics
    }

    #[tokio::test]
    async fn public_push_exporter_counts_each_start_of_overflow_once() {
        async {
            let exporter = OverflowMetricExporter::new(
                InMemoryMetricExporter::default(),
                OverflowCounting::EveryOverflowStart(OverflowTracker::default()),
            );
            let overflowing = collected(3);

            // An overflow that persists across exports is counted once.
            for _ in 0..3 {
                exporter.export(&overflowing).await.unwrap();
            }
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = OVERFLOW_METRIC
            );

            // As with delta temporality, an interval without overflow ends it, and a later
            // overflow counts again.
            exporter.export(&collected(1)).await.unwrap();
            exporter.export(&overflowing).await.unwrap();
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                2,
                "metric.name" = OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }

    /// The Apollo usage-reporting exporters keep counting every export that has overflow.
    #[tokio::test]
    async fn apollo_push_exporter_counts_every_overflowing_export() {
        async {
            let exporter = OverflowMetricExporter::new(
                InMemoryMetricExporter::default(),
                OverflowCounting::EveryExport,
            );
            let overflowing = collected(3);

            exporter.export(&overflowing).await.unwrap();
            exporter.export(&overflowing).await.unwrap();
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                2,
                "metric.name" = OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn push_exporter_does_not_count_when_counting_is_off() {
        async {
            let exporter = OverflowMetricExporter::new(
                InMemoryMetricExporter::default(),
                OverflowCounting::Off,
            );

            exporter.export(&collected(3)).await.unwrap();
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn push_exporter_does_not_count_its_own_overflow() {
        async {
            let reader = ClonableManualReader::default();
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
            let mut resource_metrics = ResourceMetrics::default();
            reader.collect(&mut resource_metrics).unwrap();

            OverflowMetricExporter::new(
                InMemoryMetricExporter::default(),
                OverflowCounting::EveryExport,
            )
            .export(&resource_metrics)
            .await
            .unwrap();
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = CARDINALITY_OVERFLOW_METRIC
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

    /// A `collect` for [`OverflowTracker::gather_and_record`] that returns [`collected`] with
    /// `values` attribute sets, and counts how often it was called.
    fn collect_with(
        values: usize,
        calls: &std::cell::Cell<usize>,
    ) -> impl FnOnce(&mut ResourceMetrics) -> OTelSdkResult + '_ {
        move |metrics| {
            calls.set(calls.get() + 1);
            *metrics = collected(values);
            Ok(())
        }
    }

    #[tokio::test]
    async fn scrapes_count_each_start_of_overflow_once() {
        async {
            let tracker = OverflowTracker::default();
            let overflowing = [scraped_family(OVERFLOW_FAMILY, true)];
            let collects = std::cell::Cell::new(0);

            // However many scrapes see the overflow, it is counted once, with the OpenTelemetry
            // name. Only the scrape where it starts collects again to find that name.
            for _ in 0..3 {
                tracker.gather_and_record(|| overflowing.to_vec(), collect_with(3, &collects));
            }
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = OVERFLOW_METRIC
            );
            assert_eq!(collects.get(), 1);

            // Once a scrape no longer shows the overflow, as can happen with an observable gauge, a
            // later one counts again.
            tracker.gather_and_record(
                || vec![scraped_family(OVERFLOW_FAMILY, false)],
                collect_with(1, &collects),
            );
            tracker.gather_and_record(|| overflowing.to_vec(), collect_with(3, &collects));
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                2,
                "metric.name" = OVERFLOW_METRIC
            );
            assert_eq!(collects.get(), 3);
        }
        .with_metrics()
        .await
    }

    #[tokio::test]
    async fn scrapes_do_not_count_without_overflow() {
        async {
            let collects = std::cell::Cell::new(0);
            OverflowTracker::default().gather_and_record(
                || vec![scraped_family(OVERFLOW_FAMILY, false)],
                collect_with(1, &collects),
            );
            assert_counter_not_exists!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                u64,
                "metric.name" = OVERFLOW_METRIC
            );
            assert_eq!(collects.get(), 0);
        }
        .with_metrics()
        .await
    }

    /// A failed collect doesn't lose the start of an overflow: the next scrape tries again.
    #[tokio::test]
    async fn scrapes_retry_naming_after_a_failed_collect() {
        async {
            let tracker = OverflowTracker::default();
            let overflowing = [scraped_family(OVERFLOW_FAMILY, true)];
            let collects = std::cell::Cell::new(0);

            tracker.gather_and_record(
                || overflowing.to_vec(),
                |_| Err(opentelemetry_sdk::error::OTelSdkError::AlreadyShutdown),
            );
            tracker.gather_and_record(|| overflowing.to_vec(), collect_with(3, &collects));
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = OVERFLOW_METRIC
            );
        }
        .with_metrics()
        .await
    }

    /// Scraper A gathers before the overflow and is delayed; scraper B arrives once it has
    /// started. B must not be compared until A has been, or A's older snapshot would end an
    /// overflow that never cleared and the next scrape would count it a second time.
    #[test]
    fn concurrent_scrapes_are_tracked_in_gather_order() {
        let tracker = &OverflowTracker::default();
        let before = &vec![scraped_family(OVERFLOW_FAMILY, false)];
        let after = &vec![scraped_family(OVERFLOW_FAMILY, true)];
        let collect_before = |metrics: &mut ResourceMetrics| {
            *metrics = collected(1);
            Ok(())
        };
        let collect_after = |metrics: &mut ResourceMetrics| {
            *metrics = collected(3);
            Ok(())
        };
        let (a_gathering, a_gathered) = std::sync::mpsc::channel();
        let (release_a, a_released) = std::sync::mpsc::channel::<()>();

        let mut started = std::thread::scope(|scope| {
            let a = scope.spawn(move || {
                tracker
                    .gather_and_track(
                        || {
                            a_gathering.send(()).unwrap();
                            a_released.recv().unwrap();
                            before.clone()
                        },
                        collect_before,
                    )
                    .1
            });
            a_gathered.recv().unwrap();
            let b =
                scope.spawn(move || tracker.gather_and_track(|| after.clone(), collect_after).1);
            // Without ordering, B would finish here while A is still delayed.
            std::thread::sleep(std::time::Duration::from_millis(100));
            release_a.send(()).unwrap();
            let mut started = a.join().unwrap();
            started.extend(b.join().unwrap());
            started
        });
        started.extend(tracker.gather_and_track(|| after.clone(), collect_after).1);

        assert_eq!(started, vec![OVERFLOW_METRIC.to_string()]);
    }

    /// A provider whose only reader is a real Prometheus exporter on `registry`, limiting `test.`
    /// instruments to two attribute sets and the overflow counter to one.
    fn prometheus_provider(
        registry: &prometheus::Registry,
    ) -> (SdkMeterProvider, SharedPrometheusExporter) {
        let exporter = SharedPrometheusExporter::from(
            opentelemetry_prometheus::exporter()
                .with_registry(registry.clone())
                .build()
                .unwrap(),
        );
        let provider = SdkMeterProvider::builder()
            .with_reader(exporter.clone())
            .with_resource(Resource::builder_empty().build())
            .with_view(|instrument: &opentelemetry_sdk::metrics::Instrument| {
                let limit = if instrument.name() == CARDINALITY_OVERFLOW_METRIC {
                    1
                } else if instrument.name().starts_with("test.") {
                    2
                } else {
                    return None;
                };
                Some(
                    Stream::builder()
                        .with_cardinality_limit(limit)
                        .build()
                        .expect("valid stream"),
                )
            })
            .build();
        (provider, exporter)
    }

    /// Against the real Prometheus exporter, whose family names differ from the OpenTelemetry
    /// names (separators, unit and `_total` suffixes), scrapes count with the OpenTelemetry name.
    #[tokio::test]
    async fn scrapes_count_with_opentelemetry_names() {
        async {
            let registry = prometheus::Registry::new();
            let (provider, exporter) = prometheus_provider(&registry);
            let meter = provider.meter("test");
            let histogram = meter
                .f64_histogram("test.request.duration")
                .with_unit("s")
                .build();
            let counter = meter.u64_counter("test.requests").build();
            for value in 0..3 {
                histogram.record(1.0, &[KeyValue::new("key", value as i64)]);
                counter.add(1, &[KeyValue::new("key", value as i64)]);
            }

            let tracker = OverflowTracker::default();
            let mut scrape = Vec::new();
            for _ in 0..3 {
                scrape = tracker
                    .gather_and_record(|| registry.gather(), |metrics| exporter.collect(metrics));
            }
            let mut families: Vec<&str> = overflowing_prometheus_names(&scrape).collect();
            families.sort_unstable();
            assert_eq!(
                families,
                ["test_request_duration_seconds", "test_requests_total"]
            );
            for metric_name in ["test.request.duration", "test.requests"] {
                assert_counter!(
                    "apollo.router.telemetry.metrics.cardinality_overflow",
                    1,
                    "metric.name" = metric_name
                );
            }
        }
        .with_metrics()
        .await
    }

    /// An observable gauge runs its callback for the scrape and again for the collect that names
    /// it. When it overflows in the scrape but not in that collect, a later scrape must still name
    /// and count it, rather than treat its family as already counted.
    #[tokio::test]
    async fn scrapes_count_a_gauge_that_the_naming_collect_missed() {
        async {
            let registry = prometheus::Registry::new();
            let (provider, exporter) = prometheus_provider(&registry);
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let _gauge = provider
                .meter("test")
                .u64_observable_gauge("test.queue.size")
                .with_callback({
                    let calls = calls.clone();
                    move |observer| {
                        // Over the limit of two on every call except the second.
                        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let values = if call == 2 { 1 } else { 3 };
                        for value in 0..values {
                            observer.observe(1, &[KeyValue::new("key", value as i64)]);
                        }
                    }
                })
                .build();

            let tracker = OverflowTracker::default();
            for _ in 0..3 {
                tracker
                    .gather_and_record(|| registry.gather(), |metrics| exporter.collect(metrics));
            }
            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1,
                "metric.name" = "test.queue.size"
            );
        }
        .with_metrics()
        .await
    }

    /// The counter's own family, as the real Prometheus exporter names it, never counts.
    #[tokio::test]
    async fn scrapes_do_not_count_their_own_overflow() {
        async {
            let registry = prometheus::Registry::new();
            let (provider, exporter) = prometheus_provider(&registry);
            let counter = provider
                .meter("test")
                .u64_counter(CARDINALITY_OVERFLOW_METRIC)
                .with_unit("count")
                .build();
            counter.add(1, &[KeyValue::new("metric.name", "a")]);
            counter.add(1, &[KeyValue::new("metric.name", "b")]);

            let scrape = registry.gather();
            assert!(
                scrape.iter().any(|family| {
                    family.name() == PROMETHEUS_CARDINALITY_OVERFLOW_FAMILY
                        && family_has_overflow(family)
                }),
                "expected the counter's own family to overflow: {scrape:?}"
            );
            assert_eq!(overflowing_prometheus_names(&scrape).count(), 0);
            let mut metrics = ResourceMetrics::default();
            exporter.collect(&mut metrics).unwrap();
            assert_eq!(overflowing_otel_names(&metrics).count(), 0);
        }
        .with_metrics()
        .await
    }
}
