//! Metrics provider construction
//!
//! This module provides tools for building OpenTelemetry meter providers from router configuration.
//!
//! ## Purpose
//!
//! The [`MetricsBuilder`] constructs meter providers for different telemetry backends:
//! - **Public metrics** - Prometheus and OTLP exporters for general observability
//! - **Apollo metrics** - Special meter providers for Apollo Studio reporting
//!
//! ## Configurator Pattern
//!
//! The [`MetricsConfigurator`] trait allows different metric exporters to contribute to the
//! builder in a uniform way. Each exporter (Prometheus, OTLP, Apollo) implements this trait
//! to extract its configuration and add appropriate readers and views to the builder.
//!
//! ## Provider Types
//!
//! Multiple meter providers are created to serve different purposes:
//! - `Public` - For standard metrics exposed via Prometheus or sent to OTLP collectors
//! - `Apollo`/`ApolloRealtime` - For metrics sent to Apollo Studio with specific filtering

use std::collections::HashSet;
use std::num::NonZeroU32;

use ahash::HashMap;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::Instrument;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::MeterProviderBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::Stream;
use prometheus::Registry;
use tower::BoxError;

use crate::_private::telemetry::ConfigResource;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricView;
use crate::plugins::telemetry::config::MetricsCommon;

/// Trait for metric exporters to contribute to meter provider construction
pub(crate) trait MetricsConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn is_enabled(&self) -> bool;
    fn configure<'a>(&self, builder: &mut MetricsBuilder<'a>) -> Result<(), BoxError>;
}

/// Builder for constructing OpenTelemetry meter providers.
///
/// Accumulates configuration from multiple exporters and builds the final meter providers
/// with appropriate readers, views, and resource attributes.
pub(crate) struct MetricsBuilder<'a> {
    pub(super) meter_provider_builders:
        HashMap<MeterProviderType, opentelemetry_sdk::metrics::MeterProviderBuilder>,
    /// Tracks which provider types have readers configured.
    /// Only providers with readers are returned from build() to avoid OTel SDK errors
    /// when observable instruments are registered with providers that have no readers.
    providers_with_readers: HashSet<MeterProviderType>,
    apollo_metrics_sender: Sender,
    prometheus_registry: Option<Registry>,
    metrics_common: &'a MetricsCommon,
    resource: Resource,
}

impl<'a> MetricsBuilder<'a> {
    pub(crate) fn build(
        self,
    ) -> (
        Option<Registry>,
        HashMap<MeterProviderType, FilterMeterProvider>,
        Sender,
    ) {
        (
            self.prometheus_registry,
            self.meter_provider_builders
                .into_iter()
                .map(|(k, v)| {
                    // Providers without readers get a noop to avoid OTel SDK errors
                    // when observable instruments are registered.
                    if !self.providers_with_readers.contains(&k) {
                        return (k, FilterMeterProvider::noop());
                    }
                    (
                        k,
                        match k {
                            MeterProviderType::Public => FilterMeterProvider::public(v.build()),
                            MeterProviderType::OtelDefault => {
                                FilterMeterProvider::public(v.build())
                            }
                            MeterProviderType::Apollo => FilterMeterProvider::apollo(v.build()),
                            MeterProviderType::ApolloRealtime => {
                                FilterMeterProvider::apollo_realtime(v.build())
                            }
                        },
                    )
                })
                .collect(),
            self.apollo_metrics_sender,
        )
    }
    pub(crate) fn configure<T: MetricsConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.is_enabled() {
            return config.configure(self);
        }
        Ok(())
    }

    pub(crate) fn new(config: &'a Conf) -> Self {
        let resource = config.exporters.metrics.common.to_resource();

        Self {
            meter_provider_builders: HashMap::default(),
            providers_with_readers: HashSet::new(),
            resource,
            apollo_metrics_sender: Sender::default(),
            prometheus_registry: None,
            metrics_common: &config.exporters.metrics.common,
        }
    }
    pub(crate) fn metrics_common(&self) -> &MetricsCommon {
        self.metrics_common
    }
    pub(crate) fn with_prometheus_registry(&mut self, prometheus_registry: Registry) -> &mut Self {
        self.prometheus_registry = Some(prometheus_registry);
        self
    }
    pub(crate) fn with_apollo_metrics_sender(
        &mut self,
        apollo_metrics_sender: Sender,
    ) -> &mut Self {
        self.apollo_metrics_sender = apollo_metrics_sender;
        self
    }
    pub(crate) fn with_reader<T: opentelemetry_sdk::metrics::reader::MetricReader>(
        &mut self,
        meter_provider_type: MeterProviderType,
        reader: T,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_reader(reader);
        // Track that this provider has a reader - only providers with readers will be returned
        self.providers_with_readers.insert(meter_provider_type);
        self
    }

    pub(crate) fn with_view<T>(
        &mut self,
        meter_provider_type: MeterProviderType,
        view: T,
    ) -> &mut Self
    where
        T: Fn(&Instrument) -> Option<Stream> + Send + Sync + 'static,
    {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_view(view);
        self
    }

    pub(crate) fn with_resource(
        &mut self,
        meter_provider_type: MeterProviderType,
        resource: Resource,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_resource(resource);
        self
    }

    /// Gets or creates a meter provider builder for a specific type.
    ///
    /// Note: Only Public and OtelDefault providers include resource attributes.
    /// Apollo providers omit resources as they're not sent to Apollo Studio.
    fn meter_provider(
        &mut self,
        meter_provider_type: MeterProviderType,
    ) -> &mut MeterProviderBuilder {
        self.meter_provider_builders
            .entry(meter_provider_type)
            .or_insert_with(|| match meter_provider_type {
                // Public and default providers include resource attributes (service name, etc.)
                MeterProviderType::Public => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                MeterProviderType::OtelDefault => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                // Apollo providers omit resource attributes - not sent to Apollo Studio
                MeterProviderType::Apollo => SdkMeterProvider::builder(),
                MeterProviderType::ApolloRealtime => SdkMeterProvider::builder(),
            })
    }

    pub(crate) fn configure_views(&mut self, meter_provider_type: MeterProviderType) {
        let bucket_boundaries = self.metrics_common().buckets.clone();
        let cardinality_limit = self.metrics_common().cardinality_limit;
        let user_views: HashMap<String, MetricView> = self
            .metrics_common()
            .views
            .clone()
            .into_iter()
            .map(|v| (v.name.clone(), v))
            .collect();

        self.with_view(meter_provider_type, move |instrument: &Instrument| {
            resolve_view(
                instrument,
                &user_views,
                &bucket_boundaries,
                cardinality_limit,
            )
        });
    }
}

/// Resolves the view [`Stream`] for an instrument by layering a user-configured
/// view (if any) on top of the globals. The histogram aggregation default is
/// seeded only for histogram instruments, never for counters or gauges.
fn resolve_view(
    instrument: &Instrument,
    user_views: &HashMap<String, MetricView>,
    bucket_boundaries: &[f64],
    cardinality_limit: Option<NonZeroU32>,
) -> Option<Stream> {
    let is_histogram = instrument.kind() == InstrumentKind::Histogram;
    let histogram_buckets = is_histogram.then(|| bucket_boundaries.to_vec());
    let user_view = user_views.get(instrument.name()).cloned();

    if histogram_buckets.is_none() && cardinality_limit.is_none() && user_view.is_none() {
        return None;
    }

    let default_view =
        MetricView::default_view(instrument.name(), histogram_buckets, cardinality_limit);
    let view = match user_view {
        Some(user) => default_view.merge(user),
        None => default_view,
    };
    Some(view.into_stream())
}

#[cfg(test)]
mod view_selection_tests {
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::data::AggregatedMetrics;
    use opentelemetry_sdk::metrics::data::MetricData;
    use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
    use opentelemetry_sdk::runtime;

    use super::*;

    const BUCKETS: &[f64] = &[0.1, 0.5, 1.0, 5.0];

    fn empty_view(name: &str) -> MetricView {
        MetricView {
            name: name.to_string(),
            rename: None,
            description: None,
            unit: None,
            aggregation: None,
            allowed_attribute_keys: None,
            cardinality_limit: None,
        }
    }

    fn meter_provider_with(
        exporter: InMemoryMetricExporter,
        user_views: Vec<MetricView>,
        global_cardinality_limit: Option<NonZeroU32>,
    ) -> SdkMeterProvider {
        let user_views: HashMap<String, MetricView> = user_views
            .into_iter()
            .map(|v| (v.name.clone(), v))
            .collect();
        let bucket_boundaries = BUCKETS.to_vec();
        MeterProviderBuilder::default()
            .with_reader(PeriodicReader::builder(exporter, runtime::Tokio).build())
            .with_view(move |instrument: &Instrument| {
                resolve_view(
                    instrument,
                    &user_views,
                    &bucket_boundaries,
                    global_cardinality_limit,
                )
            })
            .build()
    }

    /// Runs the caller's closure against the first exported metric with the given
    /// name. Panics if not found. The callback receives a borrowed `AggregatedMetrics`
    /// because the SDK type is not `Clone`.
    fn with_metric<R>(
        exporter: &InMemoryMetricExporter,
        name: &str,
        f: impl FnOnce(&AggregatedMetrics) -> R,
    ) -> R {
        let metrics = exporter
            .get_finished_metrics()
            .expect("exporter returns metrics");
        for rm in &metrics {
            for sm in rm.scope_metrics() {
                for metric in sm.metrics() {
                    if metric.name() == name {
                        return f(metric.data());
                    }
                }
            }
        }
        panic!("metric {name} not exported")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn counter_with_per_view_cardinality_limit_stays_a_counter() {
        let exporter = InMemoryMetricExporter::default();
        let mut view = empty_view("test.counter");
        view.cardinality_limit = NonZeroU32::new(1000);
        let provider = meter_provider_with(exporter.clone(), vec![view], None);

        let counter = provider.meter("t").u64_counter("test.counter").build();
        counter.add(1, &[]);
        provider.force_flush().unwrap();

        with_metric(&exporter, "test.counter", |data| {
            assert!(
                matches!(data, AggregatedMetrics::U64(MetricData::Sum(_))),
                "expected Sum aggregation, got {data:?}"
            );
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn histogram_without_user_view_uses_global_buckets() {
        let exporter = InMemoryMetricExporter::default();
        let provider = meter_provider_with(exporter.clone(), vec![], NonZeroU32::new(100));

        let histogram = provider.meter("t").f64_histogram("defaulted").build();
        histogram.record(0.3, &[]);
        provider.force_flush().unwrap();

        with_metric(&exporter, "defaulted", |data| {
            let AggregatedMetrics::F64(MetricData::Histogram(hist)) = data else {
                panic!("expected Histogram aggregation, got {data:?}")
            };
            let bounds: Vec<f64> = hist
                .data_points()
                .next()
                .map(|dp| dp.bounds().collect())
                .unwrap_or_default();
            assert_eq!(bounds, BUCKETS);
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn counter_without_user_view_or_global_limit_falls_through_to_sdk_default() {
        let exporter = InMemoryMetricExporter::default();
        let provider = meter_provider_with(exporter.clone(), vec![], None);

        let counter = provider.meter("t").u64_counter("plain.counter").build();
        counter.add(5, &[]);
        provider.force_flush().unwrap();

        with_metric(&exporter, "plain.counter", |data| {
            assert!(
                matches!(data, AggregatedMetrics::U64(MetricData::Sum(_))),
                "expected Sum aggregation, got {data:?}"
            );
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn counter_with_user_view_cardinality_limit_wins_over_global() {
        let exporter = InMemoryMetricExporter::default();
        let mut view = empty_view("limited.counter");
        view.cardinality_limit = NonZeroU32::new(2);
        let provider = meter_provider_with(exporter.clone(), vec![view], NonZeroU32::new(1000));

        let counter = provider.meter("t").u64_counter("limited.counter").build();
        counter.add(1, &[KeyValue::new("k", "a")]);
        counter.add(1, &[KeyValue::new("k", "b")]);
        counter.add(1, &[KeyValue::new("k", "c")]);
        provider.force_flush().unwrap();

        with_metric(&exporter, "limited.counter", |data| {
            let AggregatedMetrics::U64(MetricData::Sum(sum)) = data else {
                panic!("expected Sum aggregation, got {data:?}")
            };
            let has_overflow = sum.data_points().any(|dp| {
                dp.attributes()
                    .any(|kv| kv.key.as_str() == "otel.metric.overflow")
            });
            assert!(
                has_overflow,
                "per-view limit of 2 should overflow on the third attribute set; \
                 a global limit of 1000 alone would emit no overflow"
            );
        });
    }
}
