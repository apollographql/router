use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;

use derive_more::From;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::SyncInstrument;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use parking_lot::Mutex;

use crate::metrics::filter::FilterMeterProvider;

// This meter provider enables us to combine multiple meter providers. The reasons we need this are:
// 1. Prometheus meters are special. To dispose a meter is to dispose the entire registry. This means we need to make a best effort to keep them around.
// 2. To implement filtering we use a view. However this must be set during build of the meter provider, thus we need separate ones for Apollo and general metrics.
// Unlike the regular meter provider this implementation will return an existing meter if one has been created already rather than a new one.
// This is within the spec: https://opentelemetry.io/docs/specs/otel/metrics/api/#get-a-meter
// `Meters are identified by name, version, and schema_url fields. When more than one Meter of the same name, version, and schema_url is created, it is unspecified whether or under which conditions the same or different Meter instances are returned. It is a user error to create Meters with different attributes but the same identity.`

#[derive(Hash, Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug)]
pub(crate) enum MeterProviderType {
    PublicPrometheus,
    Apollo,
    ApolloRealtime,
    Public,
    OtelDefault,
}

#[derive(Clone)]
pub(crate) struct AggregateMeterProvider {
    inner: Arc<Mutex<Inner>>,
}

impl Default for AggregateMeterProvider {
    fn default() -> Self {
        let meter_provider = AggregateMeterProvider {
            inner: Arc::new(Mutex::new(Inner::default())),
        };

        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(SdkMeterProvider::default())),
        );

        meter_provider
    }
}

#[derive(Default)]
pub(crate) struct Inner {
    providers: HashMap<MeterProviderType, (FilterMeterProvider, HashMap<MeterId, Meter>)>,
    registered_instruments: Vec<InstrumentWrapper>,
}

/// Fields are never used directly but strong references here
/// keep weak references elsewhere upgradable.
#[derive(From)]
pub(crate) enum InstrumentWrapper {
    U64Counter {
        _keep_alive: Arc<Counter<u64>>,
    },
    F64Counter {
        _keep_alive: Arc<Counter<f64>>,
    },
    I64UpDownCounter {
        _keep_alive: Arc<UpDownCounter<i64>>,
    },
    F64UpDownCounter {
        _keep_alive: Arc<UpDownCounter<f64>>,
    },
    I64Histogram {
        _keep_alive: Arc<Histogram<i64>>,
    },
    U64Histogram {
        _keep_alive: Arc<Histogram<u64>>,
    },
    F64Histogram {
        _keep_alive: Arc<Histogram<f64>>,
    },
}

#[derive(Eq, PartialEq, Hash)]
struct MeterId {
    name: Cow<'static, str>,
    version: Option<Cow<'static, str>>,
    schema_url: Option<Cow<'static, str>>,
    // Note that attributes are not part of the meter ID.
}

impl AggregateMeterProvider {
    /// The behaviour of this function is that if None is passed in, the meter will be left as is.
    /// To disable meter_providers use a noop meter provider.
    /// The old meter_provider if any is returned, and it is up to the caller to clean up.
    /// Any registered instruments must be invalidated so that they are fetched again.
    pub(crate) fn set(
        &self,
        meter_provider_type: MeterProviderType,
        meter_provider: Option<FilterMeterProvider>,
    ) -> Option<FilterMeterProvider> {
        let mut inner = self.inner.lock();
        // As we are changing a meter provider we need to invalidate any registered instruments.
        // Clearing these allows any weak references at callsites to be invalidated.
        // This must be done BEFORE the old provider is dropped to ensure that metrics are not lost.
        // Once invalidated all metrics callsites will try to obtain new instruments, but will be blocked on the mutex.
        inner.registered_instruments.clear();

        //Now update the meter provider
        let old = if let Some(meter_provider) = meter_provider {
            inner
                .providers
                .insert(
                    meter_provider_type,
                    (meter_provider.clone(), HashMap::new()),
                )
                .map(|(old_provider, _)| old_provider)
        } else {
            None
        };
        // Important! The mutex MUST be dropped before the old meter provider is dropped to avoid deadlocks in the case that the export function has metrics.
        // This implicitly happens by returning the old meter provider.
        // However, to avoid a potential footgun where someone removes the return value of this function I will explicitly drop the mutex guard.
        drop(inner);

        // Important! Now it is safe to drop the old meter provider, we return it, so we should be OK. If someone removes the return value of this function then
        // this must instead be converted to a drop call.
        old
    }

    /// Shutdown MUST be called from a blocking thread.
    pub(crate) fn shutdown(&self) {
        // Make sure that we don't deadlock by dropping the mutex guard before actual shutdown happens
        // This means that if we have any misbehaving code that tries to access the meter provider during shutdown, e.g. for export metrics
        // then we don't get stuck on the mutex.
        let mut inner = self.inner.lock();
        let mut swap = Inner::default();
        std::mem::swap(&mut *inner, &mut swap);
        drop(inner);

        // Now that we have dropped the mutex guard we can safely shutdown the meter providers
        for (meter_provider_type, (meter_provider, _)) in &swap.providers {
            if let Err(e) = meter_provider.shutdown() {
                ::tracing::error!(error = %e, meter_provider_type = ?meter_provider_type, "failed to shutdown meter provider")
            }
        }
    }

    /// Create a registered instrument. This enables caching at callsites and invalidation at the meter provider via weak reference.
    pub(crate) fn create_registered_instrument<T>(
        &self,
        create_fn: impl Fn(&mut Inner) -> T,
    ) -> Arc<T>
    where
        Arc<T>: Into<InstrumentWrapper>,
    {
        let mut guard = self.inner.lock();
        let instrument = Arc::new((create_fn)(guard.deref_mut()));
        guard.registered_instruments.push(instrument.clone().into());
        instrument
    }

    #[cfg(test)]
    pub(crate) fn registered_instruments(&self) -> usize {
        self.inner.lock().registered_instruments.len()
    }
}

impl Inner {
    pub(crate) fn meter(&mut self, name: &'static str) -> Meter {
        self.versioned_meter(name, None::<Cow<'static, str>>, None::<Cow<'static, str>>, None)
    }
    pub(crate) fn versioned_meter(
        &mut self,
        name: &'static str,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        let version = version.map(|v| v.into());
        let schema_url = schema_url.map(|v| v.into());
        let mut meters = Vec::with_capacity(self.providers.len());

        for (provider, existing_meters) in self.providers.values_mut() {
            meters.push(
                existing_meters
                    .entry(MeterId {
                        name: name.into(),
                        version: version.clone(),
                        schema_url: schema_url.clone(),
                    })
                    .or_insert_with(|| {
                        let mut builder = InstrumentationScope::builder(name);
                        if let Some(ref v) = version {
                            builder = builder.with_version(v.clone());
                        }
                        if let Some(ref s) = schema_url {
                            builder = builder.with_schema_url(s.clone());
                        }
                        if let Some(ref attrs) = attributes {
                            builder = builder.with_attributes(attrs.clone());
                        }
                        provider.meter_with_scope(builder.build())
                    })
                    .clone(),
            );
        }

        Meter::new(Arc::new(AggregateInstrumentProvider { meters }))
    }
}

impl MeterProvider for AggregateMeterProvider {
    fn meter(&self, name: &'static str) -> Meter {
        let mut inner = self.inner.lock();
        inner.meter(name)
    }

    fn meter_with_scope(&self, scope: opentelemetry::InstrumentationScope) -> Meter {
        let provider = SdkMeterProvider::default();
        provider.meter_with_scope(scope)
    }
}

pub(crate) struct AggregateInstrumentProvider {
    meters: Vec<Meter>,
}

pub(crate) struct AggregateCounter<T> {
    delegates: Vec<Counter<T>>,
}

impl<T: Copy> SyncInstrument<T> for AggregateCounter<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(value, attributes)
        }
    }
}

pub(crate) struct AggregateHistogram<T> {
    delegates: Vec<Histogram<T>>,
}

impl<T: Copy> SyncInstrument<T> for AggregateHistogram<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        for histogram in &self.delegates {
            histogram.record(value, attributes)
        }
    }
}

pub(crate) struct AggregateUpDownCounter<T> {
    delegates: Vec<UpDownCounter<T>>,
}

impl<T: Copy> SyncInstrument<T> for AggregateUpDownCounter<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(value, attributes)
        }
    }
}

pub(crate) struct AggregateGauge<T> {
    delegates: Vec<Gauge<T>>,
}

impl<T: Copy> SyncInstrument<T> for AggregateGauge<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        for gauge in &self.delegates {
            gauge.record(value, attributes)
        }
    }
}

// Observable instruments don't need to have a ton of optimisation because they are only read on demand.
macro_rules! aggregate_observable_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            _builder: opentelemetry::metrics::AsyncInstrumentBuilder<'_, $wrapper<$ty>, $ty>,
        ) -> $wrapper<$ty> {
            $wrapper::new()
        }
    };
}

macro_rules! aggregate_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(
            &self,
            builder: opentelemetry::metrics::InstrumentBuilder<'_, $wrapper<$ty>>,
        ) -> $wrapper<$ty> {
            let delegates = self
                .meters
                .iter()
                .map(|p| {
                    let mut instrument_builder = p.$name(builder.name.clone());
                    if let Some(ref desc) = builder.description {
                        instrument_builder = instrument_builder.with_description(desc.clone());
                    }
                    if let Some(ref u) = builder.unit {
                        instrument_builder = instrument_builder.with_unit(u.clone());
                    }
                    instrument_builder.build()
                })
                .collect();
            $wrapper::new(Arc::new($implementation { delegates }))
        }
    };
}

macro_rules! aggregate_histogram_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(
            &self,
            builder: opentelemetry::metrics::HistogramBuilder<'_, $wrapper<$ty>>,
        ) -> $wrapper<$ty> {
            let delegates = self
                .meters
                .iter()
                .map(|p| {
                    let mut instrument_builder = p.$name(builder.name.clone());
                    if let Some(ref desc) = builder.description {
                        instrument_builder = instrument_builder.with_description(desc.clone());
                    }
                    if let Some(ref u) = builder.unit {
                        instrument_builder = instrument_builder.with_unit(u.clone());
                    }
                    instrument_builder.build()
                })
                .collect();
            $wrapper::new(Arc::new($implementation { delegates }))
        }
    };
}

impl InstrumentProvider for AggregateInstrumentProvider {
    aggregate_instrument_fn!(u64_counter, u64, Counter, AggregateCounter);
    aggregate_instrument_fn!(f64_counter, f64, Counter, AggregateCounter);

    aggregate_histogram_fn!(u64_histogram, u64, Histogram, AggregateHistogram);
    aggregate_histogram_fn!(f64_histogram, f64, Histogram, AggregateHistogram);

    aggregate_instrument_fn!(
        i64_up_down_counter,
        i64,
        UpDownCounter,
        AggregateUpDownCounter
    );
    aggregate_instrument_fn!(
        f64_up_down_counter,
        f64,
        UpDownCounter,
        AggregateUpDownCounter
    );
    aggregate_instrument_fn!(u64_gauge, u64, Gauge, AggregateGauge);
    aggregate_instrument_fn!(i64_gauge, i64, Gauge, AggregateGauge);
    aggregate_instrument_fn!(f64_gauge, f64, Gauge, AggregateGauge);

    aggregate_observable_instrument_fn!(
        i64_observable_up_down_counter,
        i64,
        ObservableUpDownCounter
    );
    aggregate_observable_instrument_fn!(
        f64_observable_up_down_counter,
        f64,
        ObservableUpDownCounter
    );

    aggregate_observable_instrument_fn!(f64_observable_gauge, f64, ObservableGauge);
    aggregate_observable_instrument_fn!(i64_observable_gauge, i64, ObservableGauge);
    aggregate_observable_instrument_fn!(u64_observable_gauge, u64, ObservableGauge);
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use crate::metrics::aggregation::AggregateMeterProvider;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::filter::FilterMeterProvider;
    use async_trait::async_trait;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::PeriodicReader;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::Gauge;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::reader::MetricReader;

    #[derive(Clone, Debug)]
    struct SharedReader(Arc<ManualReader>);

    #[test]
    fn test_i64_gauge_drop() {
        let reader = SharedReader(Arc::new(ManualReader::builder().build()));

        let delegate = MeterProviderBuilder::default()
            .with_reader(reader.clone())
            .build();
        let meter_provider = AggregateMeterProvider::default();
        meter_provider.set(
            MeterProviderType::Public,
            Some(FilterMeterProvider::public(delegate)),
        );
        let meter = meter_provider.meter("test");

        let observe_counter = Arc::new(AtomicI64::new(0));
        let callback_observe_counter = observe_counter.clone();
        let gauge = meter
            .i64_observable_gauge("test")
            .with_callback(move |i| {
                let count =
                    callback_observe_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                i.observe(count + 1, &[])
            })
            .init();

        let mut result = ResourceMetrics::default();

        // Fetching twice will call the observer twice
        reader
            .collect(&mut result)
            .expect("metrics must be collected");
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(get_gauge_value(&mut result), 2);

        // Dropping the gauge should remove the observer registration
        drop(gauge);

        // No further increment will happen
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(observe_counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn test_i64_gauge_lifecycle() {
        let reader = SharedReader(Arc::new(ManualReader::builder().build()));

        let delegate = MeterProviderBuilder::default()
            .with_reader(reader.clone())
            .build();
        let meter_provider = AggregateMeterProvider::default();
        meter_provider.set(
            MeterProviderType::Public,
            Some(FilterMeterProvider::public(delegate)),
        );
        let meter = meter_provider.meter("test");

        let observe_counter = Arc::new(AtomicI64::new(0));
        let callback_observe_counter1 = observe_counter.clone();
        let callback_observe_counter2 = observe_counter.clone();
        let gauge1 = meter
            .i64_observable_gauge("test")
            .with_callback(move |i| {
                let count =
                    callback_observe_counter1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                i.observe(count + 1, &[])
            })
            .init();

        let mut result = ResourceMetrics::default();

        // Fetching metrics will call the observer
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(get_gauge_value(&mut result), 1);
        drop(gauge1);

        // The first gauge is dropped, let's create a new one
        let gauge2 = meter
            .i64_observable_gauge("test")
            .with_callback(move |i| {
                let count =
                    callback_observe_counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                i.observe(count + 1, &[])
            })
            .init();

        // Fetching metrics will call the observer ONLY on the remaining gauge
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(get_gauge_value(&mut result), 2);
        drop(gauge2);
    }

    fn get_gauge_value(result: &mut ResourceMetrics) -> i64 {
        assert_eq!(result.scope_metrics().len(), 1);
        assert_eq!(result.scope_metrics().first().unwrap().metrics().len(), 1);
        assert_eq!(
            result
                .scope_metrics()
                .first()
                .unwrap()
                .metrics
                .first()
                .unwrap()
                .data
                .as_any()
                .downcast_ref::<Gauge<i64>>()
                .unwrap()
                .data_points
                .len(),
            1
        );
        result
            .scope_metrics()
            .first()
            .unwrap()
            .metrics
            .first()
            .unwrap()
            .data
            .as_any()
            .downcast_ref::<Gauge<i64>>()
            .unwrap()
            .data_points
            .first()
            .unwrap()
            .value
    }

    #[test]
    fn test_global_meter_provider() {
        // The global meter provider is populated in AggregateMeterProvider::Default, but we can't test that without interacting with statics.
        // Setting it explicitly is the next best thing.

        let reader = SharedReader(Arc::new(ManualReader::builder().build()));

        let delegate = MeterProviderBuilder::default()
            .with_reader(reader.clone())
            .build();

        let meter_provider = AggregateMeterProvider::default();
        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(delegate)),
        );

        let counter = meter_provider
            .versioned_meter("test", None::<String>, None::<String>, None)
            .u64_counter("test.counter")
            .init();
        counter.add(1, &[]);
        let mut resource_metrics = ResourceMetrics::default();
        reader.collect(&mut resource_metrics).unwrap();
        assert_eq!(1, resource_metrics.scope_metrics().len());
    }

    struct TestExporter {
        meter_provider: AggregateMeterProvider,
        shutdown: Arc<AtomicBool>,
    }

    #[async_trait]
    impl PushMetricExporter for TestExporter {
        fn export(
            &self,
            _metrics: &ResourceMetrics,
        ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
            async move {
                self.count();
                Ok(())
            }
        }

        fn force_flush(&self) -> OTelSdkResult {
            self.count();
            Ok(())
        }

        fn shutdown(&self) -> OTelSdkResult {
            self.count();
            self.shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            self.count();
            self.shutdown_with_timeout(_timeout);
            Ok(())
        }
    }

    impl TestExporter {
        fn count(&self) {
            let counter = self
                .meter_provider
                .versioned_meter("test", None::<String>, None::<String>, None)
                .u64_counter("test.counter")
                .init();
            counter.add(1, &[]);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_shutdown_exporter_metrics() {
        // See the `shutdown` method implementation as to why this is tricky.
        // This test calls the meter provider from within the exporter to ensure there is no deadlock possible.
        let meter_provider = AggregateMeterProvider::default();
        let shutdown = Arc::new(AtomicBool::new(false));
        let periodic_reader = reader(&meter_provider, &shutdown);

        let delegate = MeterProviderBuilder::default()
            .with_reader(periodic_reader)
            .build();

        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(delegate)),
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        meter_provider.shutdown();

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(shutdown.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reload_exporter_metrics() {
        // When exporters that interact with the meter provider are being refreshed we want to ensure that they don't deadlock.
        // I don't think that this could have ever happened, but best to be safe and add a test.
        let meter_provider = AggregateMeterProvider::default();
        let shutdown1 = Arc::new(AtomicBool::new(false));
        let periodic_reader = reader(&meter_provider, &shutdown1);

        let delegate = MeterProviderBuilder::default()
            .with_reader(periodic_reader)
            .build();

        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(delegate)),
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        let shutdown2 = Arc::new(AtomicBool::new(false));
        let periodic_reader = reader(&meter_provider, &shutdown2);

        let delegate = MeterProviderBuilder::default()
            .with_reader(periodic_reader)
            .build();

        // Setting the meter provider should not deadlock.
        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(delegate)),
        );

        tokio::time::sleep(Duration::from_millis(20)).await;

        // The first meter provider should be shut down and the second is still active
        assert!(shutdown1.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!shutdown2.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn reader(
        meter_provider: &AggregateMeterProvider,
        shutdown: &Arc<AtomicBool>,
    ) -> PeriodicReader<TestExporter> {
        PeriodicReader::builder(TestExporter {
            meter_provider: meter_provider.clone(),
            shutdown: shutdown.clone(),
        })
        .with_interval(Duration::from_millis(10))
        .with_timeout(Duration::from_millis(10))
        .build()
    }
}
