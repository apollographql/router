use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::mem;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::Mutex;

use derive_more::From;
use itertools::Itertools;
use opentelemetry::metrics::Callback;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::SyncCounter;
use opentelemetry::metrics::SyncHistogram;
use opentelemetry::metrics::SyncUpDownCounter;
use opentelemetry::metrics::Unit;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry::KeyValue;
use opentelemetry_api::metrics::AsyncInstrument;
use opentelemetry_api::metrics::CallbackRegistration;
use opentelemetry_api::metrics::MetricsError;
use opentelemetry_api::metrics::Observer;

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

        // If the regular global meter provider has been set then the aggregate meter provider will use it. Otherwise it'll default to a no-op.
        // For this to work the global meter provider must be set before the aggregate meter provider is created.
        // This functionality is not guaranteed to stay like this, so use at your own risk.
        meter_provider.set(
            MeterProviderType::OtelDefault,
            Some(FilterMeterProvider::public(
                opentelemetry_api::global::meter_provider(),
            )),
        );

        meter_provider
    }
}

#[derive(Default)]
pub(crate) struct Inner {
    providers: HashMap<MeterProviderType, (FilterMeterProvider, HashMap<MeterId, Meter>)>,
    registered_instruments: Vec<InstrumentWrapper>,
}

#[derive(From)]
pub(crate) enum InstrumentWrapper {
    U64Counter(Arc<Counter<u64>>),
    F64Counter(Arc<Counter<f64>>),
    I64UpDownCounter(Arc<UpDownCounter<i64>>),
    F64UpDownCounter(Arc<UpDownCounter<f64>>),
    I64Histogram(Arc<Histogram<i64>>),
    U64Histogram(Arc<Histogram<u64>>),
    F64Histogram(Arc<Histogram<f64>>),
    U64Gauge(Arc<ObservableGauge<u64>>),
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
        let mut inner = self.inner.lock().expect("lock poisoned");
        // As we are changing a meter provider we need to invalidate any registered instruments.
        // Clearing these allows any weak references at callsites to be invalidated.
        inner.registered_instruments.clear();

        //Now update the meter provider
        if let Some(meter_provider) = meter_provider {
            inner
                .providers
                .insert(
                    meter_provider_type,
                    (meter_provider.clone(), HashMap::new()),
                )
                .map(|(old_provider, _)| old_provider)
        } else {
            None
        }
    }

    /// Shutdown MUST be called from a blocking thread.
    pub(crate) fn shutdown(&self) {
        let inner = self.inner.lock().expect("lock poisoned");
        for (meter_provider_type, (meter_provider, _)) in &inner.providers {
            if let Err(e) = meter_provider.shutdown() {
                ::tracing::error!(error = %e, meter_provider_type = ?meter_provider_type, "failed to shutdown meter provider")
            }
        }
    }

    /// Create a registered instrument. This enables caching at callsites and invalidation at the meter provider via weak reference.
    #[allow(dead_code)]
    pub(crate) fn create_registered_instrument<T>(
        &self,
        create_fn: impl Fn(&mut Inner) -> T,
    ) -> Arc<T>
    where
        Arc<T>: Into<InstrumentWrapper>,
    {
        let mut guard = self.inner.lock().expect("lock poisoned");
        let instrument = Arc::new((create_fn)(guard.deref_mut()));
        guard.registered_instruments.push(instrument.clone().into());
        instrument
    }

    #[cfg(test)]
    pub(crate) fn registered_instruments(&self) -> usize {
        self.inner
            .lock()
            .expect("lock poisoned")
            .registered_instruments
            .len()
    }
}

impl Inner {
    pub(crate) fn meter(&mut self, name: impl Into<Cow<'static, str>>) -> Meter {
        self.versioned_meter(
            name,
            None::<Cow<'static, str>>,
            None::<Cow<'static, str>>,
            None,
        )
    }
    pub(crate) fn versioned_meter(
        &mut self,
        name: impl Into<Cow<'static, str>>,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        let name = name.into();
        let version = version.map(|v| v.into());
        let schema_url = schema_url.map(|v| v.into());
        let mut meters = Vec::with_capacity(self.providers.len());

        for (provider, existing_meters) in self.providers.values_mut() {
            meters.push(
                existing_meters
                    .entry(MeterId {
                        name: name.clone(),
                        version: version.clone(),
                        schema_url: schema_url.clone(),
                    })
                    .or_insert_with(|| {
                        provider.versioned_meter(
                            name.clone(),
                            version.clone(),
                            schema_url.clone(),
                            attributes.clone(),
                        )
                    })
                    .clone(),
            );
        }

        Meter::new(Arc::new(AggregateInstrumentProvider { meters }))
    }
}

impl MeterProvider for AggregateMeterProvider {
    fn versioned_meter(
        &self,
        name: impl Into<Cow<'static, str>>,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        let mut inner = self.inner.lock().expect("lock poisoned");
        inner.versioned_meter(name, version, schema_url, attributes)
    }
}

pub(crate) struct AggregateInstrumentProvider {
    meters: Vec<Meter>,
}

pub(crate) struct AggregateCounter<T> {
    delegates: Vec<Counter<T>>,
}

impl<T: Copy> SyncCounter<T> for AggregateCounter<T> {
    fn add(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(value, attributes)
        }
    }
}

pub(crate) struct AggregateObservableCounter<T> {
    delegates: Vec<(ObservableCounter<T>, Option<DroppingUnregister>)>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableCounter<T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        for (counter, _) in &self.delegates {
            counter.observe(value, attributes)
        }
    }

    fn as_any(&self) -> Arc<dyn Any> {
        unreachable!()
    }
}

pub(crate) struct AggregateHistogram<T> {
    delegates: Vec<Histogram<T>>,
}

impl<T: Copy> SyncHistogram<T> for AggregateHistogram<T> {
    fn record(&self, value: T, attributes: &[KeyValue]) {
        for histogram in &self.delegates {
            histogram.record(value, attributes)
        }
    }
}

pub(crate) struct AggregateUpDownCounter<T> {
    delegates: Vec<UpDownCounter<T>>,
}

impl<T: Copy> SyncUpDownCounter<T> for AggregateUpDownCounter<T> {
    fn add(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(value, attributes)
        }
    }
}

pub(crate) struct AggregateObservableUpDownCounter<T> {
    delegates: Vec<(ObservableUpDownCounter<T>, Option<DroppingUnregister>)>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableUpDownCounter<T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        for (counter, _) in &self.delegates {
            counter.observe(value, attributes)
        }
    }

    fn as_any(&self) -> Arc<dyn Any> {
        unreachable!()
    }
}

pub(crate) struct AggregateObservableGauge<T> {
    delegates: Vec<(ObservableGauge<T>, Option<DroppingUnregister>)>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableGauge<T> {
    fn observe(&self, measurement: T, attributes: &[KeyValue]) {
        for (gauge, _) in &self.delegates {
            gauge.observe(measurement, attributes)
        }
    }

    fn as_any(&self) -> Arc<dyn Any> {
        unreachable!()
    }
}
// Observable instruments don't need to have a ton of optimisation because they are only read on demand.
macro_rules! aggregate_observable_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(
            &self,
            name: Cow<'static, str>,
            description: Option<Cow<'static, str>>,
            unit: Option<Unit>,
            callback: Vec<Callback<$ty>>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let callback: Vec<Arc<Callback<$ty>>> =
                callback.into_iter().map(|c| Arc::new(c)).collect_vec();
            let delegates = self
                .meters
                .iter()
                .map(|meter| {
                    let mut builder = meter.$name(name.clone());
                    if let Some(description) = &description {
                        builder = builder.with_description(description.clone());
                    }
                    if let Some(unit) = &unit {
                        builder = builder.with_unit(unit.clone());
                    }
                    // We must not set callback in the builder as it will leak memory.
                    // Instead we use callback registration on the meter provider as it allows unregistration
                    // Also we need to filter out no-op instruments as passing these to the meter provider as these will fail witha crptic message about different implementation.
                    // Confusingly the implementation of as_any() on an instrument will return 'other stuff'. In particular no-ops return Arc<()>. This is why we need to check for this.
                    let delegate: $wrapper<$ty> = builder.try_init()?;
                    let registration = if delegate.clone().as_any().downcast_ref::<()>().is_some() {
                        // This is a no-op instrument, so we don't need to register a callback.
                        None
                    } else {
                        let delegate = delegate.clone();
                        let callback = callback.clone();
                        Some(
                            meter.register_callback(&[delegate.clone().as_any()], move |_| {
                                for callback in &callback {
                                    callback(&delegate);
                                }
                            })?,
                        )
                    };
                    let result: opentelemetry::metrics::Result<_> =
                        Ok((delegate, registration.map(DroppingUnregister)));
                    result
                })
                .try_collect()?;
            Ok($wrapper::new(Arc::new($implementation { delegates })))
        }
    };
}

struct DroppingUnregister(Box<dyn CallbackRegistration>);

macro_rules! aggregate_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(
            &self,
            name: Cow<'static, str>,
            description: Option<Cow<'static, str>>,
            unit: Option<Unit>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let delegates = self
                .meters
                .iter()
                .map(|p| {
                    let mut b = p.$name(name.clone());
                    if let Some(description) = &description {
                        b = b.with_description(description.clone());
                    }
                    if let Some(unit) = &unit {
                        b = b.with_unit(unit.clone());
                    }
                    b.try_init()
                })
                .try_collect()?;
            Ok($wrapper::new(Arc::new($implementation { delegates })))
        }
    };
}
impl Drop for DroppingUnregister {
    fn drop(&mut self) {
        if let Err(e) = self.0.unregister() {
            ::tracing::error!(error = %e, "failed to unregister callback")
        }
    }
}

impl InstrumentProvider for AggregateInstrumentProvider {
    aggregate_instrument_fn!(u64_counter, u64, Counter, AggregateCounter);
    aggregate_instrument_fn!(f64_counter, f64, Counter, AggregateCounter);

    aggregate_observable_instrument_fn!(
        f64_observable_counter,
        f64,
        ObservableCounter,
        AggregateObservableCounter
    );
    aggregate_observable_instrument_fn!(
        u64_observable_counter,
        u64,
        ObservableCounter,
        AggregateObservableCounter
    );

    aggregate_instrument_fn!(u64_histogram, u64, Histogram, AggregateHistogram);
    aggregate_instrument_fn!(f64_histogram, f64, Histogram, AggregateHistogram);
    aggregate_instrument_fn!(i64_histogram, i64, Histogram, AggregateHistogram);

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

    aggregate_observable_instrument_fn!(
        i64_observable_up_down_counter,
        i64,
        ObservableUpDownCounter,
        AggregateObservableUpDownCounter
    );
    aggregate_observable_instrument_fn!(
        f64_observable_up_down_counter,
        f64,
        ObservableUpDownCounter,
        AggregateObservableUpDownCounter
    );

    aggregate_observable_instrument_fn!(
        f64_observable_gauge,
        f64,
        ObservableGauge,
        AggregateObservableGauge
    );
    aggregate_observable_instrument_fn!(
        i64_observable_gauge,
        i64,
        ObservableGauge,
        AggregateObservableGauge
    );
    aggregate_observable_instrument_fn!(
        u64_observable_gauge,
        u64,
        ObservableGauge,
        AggregateObservableGauge
    );

    fn register_callback(
        &self,
        _instruments: &[Arc<dyn Any>],
        _callbacks: Box<dyn Fn(&dyn Observer) + Send + Sync>,
    ) -> opentelemetry_api::metrics::Result<Box<dyn CallbackRegistration>> {
        // We may implement this in future, but for now we don't need it and it's a pain to implement because we need to unwrap the aggregate instruments and pass them to the meter provider that owns them.
        unimplemented!("register_callback is not supported on AggregateInstrumentProvider");
    }
}

struct AggregatedCallbackRegistrations(Vec<Box<dyn CallbackRegistration>>);
impl CallbackRegistration for AggregatedCallbackRegistrations {
    fn unregister(&mut self) -> opentelemetry_api::metrics::Result<()> {
        let mut errors = vec![];
        for mut registration in mem::take(&mut self.0) {
            if let Err(err) = registration.unregister() {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(MetricsError::Other(format!("{errors:?}")))
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::AtomicI64;
    use std::sync::Arc;
    use std::sync::Weak;

    use opentelemetry::sdk::metrics::data::Gauge;
    use opentelemetry::sdk::metrics::data::ResourceMetrics;
    use opentelemetry::sdk::metrics::data::Temporality;
    use opentelemetry::sdk::metrics::reader::AggregationSelector;
    use opentelemetry::sdk::metrics::reader::MetricProducer;
    use opentelemetry::sdk::metrics::reader::MetricReader;
    use opentelemetry::sdk::metrics::reader::TemporalitySelector;
    use opentelemetry::sdk::metrics::Aggregation;
    use opentelemetry::sdk::metrics::InstrumentKind;
    use opentelemetry::sdk::metrics::ManualReader;
    use opentelemetry::sdk::metrics::MeterProviderBuilder;
    use opentelemetry::sdk::metrics::Pipeline;
    use opentelemetry_api::global::GlobalMeterProvider;
    use opentelemetry_api::metrics::MeterProvider;
    use opentelemetry_api::metrics::Result;
    use opentelemetry_api::Context;

    use crate::metrics::aggregation::AggregateMeterProvider;
    use crate::metrics::aggregation::MeterProviderType;
    use crate::metrics::filter::FilterMeterProvider;

    #[derive(Clone, Debug)]
    struct SharedReader(Arc<ManualReader>);

    impl TemporalitySelector for SharedReader {
        fn temporality(&self, kind: InstrumentKind) -> Temporality {
            self.0.temporality(kind)
        }
    }

    impl AggregationSelector for SharedReader {
        fn aggregation(&self, kind: InstrumentKind) -> Aggregation {
            self.0.aggregation(kind)
        }
    }

    impl MetricReader for SharedReader {
        fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
            self.0.register_pipeline(pipeline)
        }

        fn register_producer(&self, producer: Box<dyn MetricProducer>) {
            self.0.register_producer(producer)
        }

        fn collect(&self, rm: &mut ResourceMetrics) -> Result<()> {
            self.0.collect(rm)
        }

        fn force_flush(&self, cx: &Context) -> Result<()> {
            self.0.force_flush(cx)
        }

        fn shutdown(&self) -> Result<()> {
            self.0.shutdown()
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

        let mut result = ResourceMetrics {
            resource: Default::default(),
            scope_metrics: Default::default(),
        };

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

        let mut result = ResourceMetrics {
            resource: Default::default(),
            scope_metrics: Default::default(),
        };

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
        assert_eq!(result.scope_metrics.len(), 1);
        assert_eq!(result.scope_metrics.first().unwrap().metrics.len(), 1);
        assert_eq!(
            result
                .scope_metrics
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
            .scope_metrics
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
            Some(FilterMeterProvider::public(GlobalMeterProvider::new(
                delegate,
            ))),
        );

        let counter = meter_provider
            .versioned_meter("test", None::<String>, None::<String>, None)
            .u64_counter("test.counter")
            .init();
        counter.add(1, &[]);
        let mut resource_metrics = ResourceMetrics {
            resource: Default::default(),
            scope_metrics: vec![],
        };
        reader.collect(&mut resource_metrics).unwrap();
        assert_eq!(1, resource_metrics.scope_metrics.len());
    }
}
