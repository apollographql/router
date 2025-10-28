use std::borrow::Cow;
use std::collections::HashMap;
use std::mem;
use std::mem::take;
use std::sync::Arc;

use derive_more::From;
use itertools::Itertools;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Callback;
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
use strum::EnumCount;
use strum_macros::Display;
use strum_macros::EnumCount;
use strum_macros::EnumIter;

use crate::metrics::filter::FilterMeterProvider;

// This meter provider enables us to combine multiple meter providers. The reasons we need this are:
// 1. Prometheus meters are special. To dispose a meter is to dispose the entire registry. This means we need to make a best effort to keep them around.
// 2. To implement filtering we use a view. However this must be set during build of the meter provider, thus we need separate ones for Apollo and general metrics.
// Unlike the regular meter provider this implementation will return an existing meter if one has been created already rather than a new one.
// This is within the spec: https://opentelemetry.io/docs/specs/otel/metrics/api/#get-a-meter
// `Meters are identified by name, version, and schema_url fields. When more than one Meter of the same name, version, and schema_url is created, it is unspecified whether or under which conditions the same or different Meter instances are returned. It is a user error to create Meters with different attributes but the same identity.`

#[derive(
    Hash, Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug, EnumCount, EnumIter, Display,
)]
#[repr(u8)]
pub(crate) enum MeterProviderType {
    Apollo,
    ApolloRealtime,
    Public,
    OtelDefault,
}

#[derive(Clone)]
pub(crate) struct AggregateMeterProvider {
    inner: Arc<Mutex<Option<Inner>>>,
}

impl Default for AggregateMeterProvider {
    fn default() -> Self {
        let meter_provider = AggregateMeterProvider {
            inner: Arc::new(Mutex::new(Some(Inner::default()))),
        };

        meter_provider.set(
            MeterProviderType::OtelDefault,
            FilterMeterProvider::public(SdkMeterProvider::default()),
        );

        meter_provider
    }
}

pub(crate) struct Inner {
    providers: Vec<(FilterMeterProvider, HashMap<MeterId, Meter>)>,
    registered_instruments: Vec<InstrumentWrapper>,
}

impl Default for Inner {
    fn default() -> Self {
        Inner {
            providers: (0..MeterProviderType::COUNT)
                .map(|_| {
                    (
                        FilterMeterProvider::public(SdkMeterProvider::default()),
                        HashMap::new(),
                    )
                })
                .collect(),
            registered_instruments: Vec::new(),
        }
    }
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

#[derive(Eq, PartialEq, Hash, Clone)]
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
        meter_provider: FilterMeterProvider,
    ) -> FilterMeterProvider {
        let mut guard = self.inner.lock();
        let inner = guard
            .as_mut()
            .expect("cannot use meter provider after shutdown");
        // As we are changing a meter provider we need to invalidate any registered instruments.
        // Clearing these allows any weak references at callsites to be invalidated.
        // This must be done BEFORE the old provider is dropped to ensure that metrics are not lost.
        // Once invalidated all metrics callsites will try to obtain new instruments, but will be blocked on the mutex.
        inner.invalidate();

        //Now update the meter provider
        let mut swap = (meter_provider, HashMap::new());
        mem::swap(
            &mut inner.providers[meter_provider_type as usize],
            &mut swap,
        );

        // Important! The mutex MUST be dropped before the old meter provider is dropped to avoid deadlocks in the case that the export function has metrics.
        // This implicitly happens by returning the old meter provider.
        // However, to avoid a potential footgun where someone removes the return value of this function I will explicitly drop the mutex guard.
        drop(guard);

        // Important! Now it is safe to drop the old meter provider, we return it, so we should be OK. If someone removes the return value of this function then
        // this must instead be converted to a drop call.
        swap.0
    }

    /// Invalidate all the cached instruments
    #[cfg(test)]
    pub(crate) fn invalidate(&self) {
        if let Some(inner) = self.inner.lock().as_mut() {
            inner.invalidate();
        }
    }

    /// Shutdown MUST be called from a blocking thread.
    pub(crate) fn shutdown(&self) {
        // Make sure that we don't deadlock by dropping the mutex guard before actual shutdown happens
        // This means that if we have any misbehaving code that tries to access the meter provider during shutdown, e.g. for export metrics
        // then we don't get stuck on the mutex.
        // For instance the apollo exporters have in the past had metrics for exporting, as
        // they shut down they try to increment a metric which causes a new meter to be created.
        // However, if we have not released the guard then we deadlock.
        let mut guard = self.inner.lock();
        let old = guard.take();
        drop(guard);
        drop(old);
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
        let inner = guard
            .as_mut()
            .expect("cannot use meter provider after shutdown");
        inner.create_registered_instrument(create_fn)
    }

    #[cfg(test)]
    pub(crate) fn registered_instruments(&self) -> usize {
        self.inner
            .lock()
            .as_ref()
            .expect("cannot use meter provider after shutdown")
            .registered_instruments
            .len()
    }
}

impl Inner {
    pub(crate) fn invalidate(&mut self) {
        self.registered_instruments.clear()
    }
    pub(crate) fn meter(&mut self, name: &'static str) -> Meter {
        self.versioned_meter(
            name,
            None::<Cow<'static, str>>,
            None::<Cow<'static, str>>,
            None,
        )
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

        for (provider, existing_meters) in &mut self.providers {
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

    pub(crate) fn create_registered_instrument<T>(
        &mut self,
        create_fn: impl Fn(&mut Inner) -> T,
    ) -> Arc<T>
    where
        Arc<T>: Into<InstrumentWrapper>,
    {
        let instrument = Arc::new((create_fn)(self));
        self.registered_instruments.push(instrument.clone().into());
        instrument
    }
}

impl MeterProvider for AggregateMeterProvider {
    fn meter(&self, name: &'static str) -> Meter {
        let mut inner = self.inner.lock();
        if let Some(inner) = inner.as_mut() {
            inner.meter(name)
        } else {
            // The meter was used after shutdown. Fall back to a meter from a provider with no
            // readers since the instrument cannot actually be used
            SdkMeterProvider::default().meter(name)
        }
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
    ($name:ident, $ty:ty, $instrument:ident) => {
        fn $name(
            &self,
            mut builder: opentelemetry::metrics::AsyncInstrumentBuilder<'_, $instrument<$ty>, $ty>,
        ) -> $instrument<$ty> {
            let callbacks: Vec<Arc<Callback<$ty>>> = take(&mut builder.callbacks)
                .into_iter()
                .map(Arc::from)
                .collect_vec();
            let name = builder.name.clone();
            let description = builder.description.clone();
            let unit = builder.unit.clone();

            // Build the originally defined instrument for each meter. Most importantly, this will
            // register the callbacks
            let mut handles = Vec::with_capacity(self.meters.len());
            for meter in &self.meters {
                let mut b = meter.$name(name.clone());
                if let Some(ref d) = description {
                    b = b.with_description(d.clone());
                }
                if let Some(ref u) = unit {
                    b = b.with_unit(u.clone());
                }
                for cb in &callbacks {
                    let cb = Arc::clone(cb);
                    b = b.with_callback(move |inst| cb(inst));
                }
                handles.push(b.build());
            }

            $instrument::new()
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
    use std::sync::Weak;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use opentelemetry::InstrumentationScope;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::PeriodicReader;
    use opentelemetry_sdk::metrics::Pipeline;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::reader::MetricReader;

    use crate::metrics::aggregation::AggregateMeterProvider;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::filter::FilterMeterProvider;

    #[derive(Clone, Debug)]
    struct SharedReader(Arc<ManualReader>);

    impl MetricReader for SharedReader {
        fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
            self.0.register_pipeline(pipeline)
        }

        fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
            self.0.collect(rm)
        }

        fn force_flush(&self) -> OTelSdkResult {
            self.0.force_flush()
        }

        fn shutdown(&self) -> OTelSdkResult {
            self.0.shutdown()
        }

        fn temporality(&self, _kind: opentelemetry_sdk::metrics::InstrumentKind) -> Temporality {
            Temporality::Cumulative
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            self.shutdown()
        }
    }

    #[test]
    fn test_i64_gauge_drop() {
        let reader = SharedReader(Arc::new(ManualReader::builder().build()));

        let delegate = MeterProviderBuilder::default()
            .with_reader(reader.clone())
            .build();
        let meter_provider = AggregateMeterProvider::default();
        meter_provider.set(
            MeterProviderType::Public,
            FilterMeterProvider::public(delegate),
        );
        let meter = meter_provider.meter("test");

        let observe_counter = Arc::new(AtomicI64::new(0));
        let callback_observe_counter = observe_counter.clone();

        let mut result = ResourceMetrics::default();
        {
            let _gauge = meter
                .i64_observable_gauge("test")
                .with_callback(move |i| {
                    let count =
                        callback_observe_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    i.observe(count + 1, &[])
                })
                .build();

            // Fetching twice will call the observer twice
            reader
                .collect(&mut result)
                .expect("metrics must be collected");
            reader
                .collect(&mut result)
                .expect("metrics must be collected");

            assert_eq!(get_gauge_value(&mut result), 2);
        } // Limited scope to drop the gauge after use (b/c it does not impl drop)

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
            FilterMeterProvider::public(delegate),
        );
        let meter = meter_provider.meter("test");

        let observe_counter = Arc::new(AtomicI64::new(0));
        let callback_observe_counter1 = observe_counter.clone();
        let callback_observe_counter2 = observe_counter.clone();

        let mut result = ResourceMetrics::default();

        {
            let _gauge1 = meter
                .i64_observable_gauge("test")
                .with_callback(move |i| {
                    let count =
                        callback_observe_counter1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    i.observe(count + 1, &[])
                })
                .build();

            // Fetching metrics will call the observer
            reader
                .collect(&mut result)
                .expect("metrics must be collected");

            assert_eq!(get_gauge_value(&mut result), 1);
        } // Limited scope to drop the gauge after use (b/c it does not impl drop)

        {
            // The first gauge is dropped, let's create a new one
            let _gauge2 = meter
                .i64_observable_gauge("test")
                .with_callback(move |i| {
                    let count =
                        callback_observe_counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    i.observe(count + 1, &[])
                })
                .build();

            // Fetching metrics will call the observer ONLY on the remaining gauge
            reader
                .collect(&mut result)
                .expect("metrics must be collected");

            assert_eq!(get_gauge_value(&mut result), 2);
        } // Limited scope to drop the gauge after use (b/c it does not impl drop)
    }

    fn get_gauge_value(result: &mut ResourceMetrics) -> i64 {
        let scope_metrics: Vec<_> = result.scope_metrics().collect();
        assert_eq!(scope_metrics.len(), 1);

        let metrics: Vec<_> = scope_metrics.first().unwrap().metrics().collect();
        assert_eq!(metrics.len(), 1);

        let metric = metrics.first().unwrap();

        match metric.data() {
            opentelemetry_sdk::metrics::data::AggregatedMetrics::F64(_metric_data) => {
                panic!("Expected i64 gauge metric")
            }
            opentelemetry_sdk::metrics::data::AggregatedMetrics::U64(_metric_data) => {
                panic!("Expected i64 gauge metric")
            }
            opentelemetry_sdk::metrics::data::AggregatedMetrics::I64(metric_data) => {
                match metric_data {
                    opentelemetry_sdk::metrics::data::MetricData::Gauge(gauge) => {
                        gauge.data_points().next().unwrap().value()
                    }
                    _ => panic!("Expected gauge metric"),
                }
            }
        }
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
            FilterMeterProvider::public(delegate),
        );

        let counter = meter_provider
            .meter_with_scope(InstrumentationScope::builder("test").build())
            .u64_counter("test.counter")
            .build();
        counter.add(1, &[]);
        let mut resource_metrics = ResourceMetrics::default();
        reader.collect(&mut resource_metrics).unwrap();
        assert_eq!(1, resource_metrics.scope_metrics().count());
    }

    struct TestExporter {
        meter_provider: AggregateMeterProvider,
        shutdown: Arc<AtomicBool>,
    }

    impl PushMetricExporter for TestExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            self.count();
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            self.count();
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            self.count();
            self.shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
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
    }

    impl TestExporter {
        fn count(&self) {
            let counter = self
                .meter_provider
                .meter_with_scope(InstrumentationScope::builder("test").build())
                .u64_counter("test.counter")
                .build();
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
            FilterMeterProvider::public(delegate),
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
            FilterMeterProvider::public(delegate),
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
            FilterMeterProvider::public(delegate),
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
        .build()
    }
}
