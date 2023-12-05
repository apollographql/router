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
}

#[derive(Clone, Default)]
pub(crate) struct AggregateMeterProvider {
    inner: Arc<Mutex<Inner>>,
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
    delegates: Vec<ObservableCounter<T>>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableCounter<T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
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
    delegates: Vec<ObservableUpDownCounter<T>>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableUpDownCounter<T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.observe(value, attributes)
        }
    }

    fn as_any(&self) -> Arc<dyn Any> {
        unreachable!()
    }
}

pub(crate) struct AggregateObservableGauge<T> {
    delegates: Vec<ObservableGauge<T>>,
}

impl<T: Copy> AsyncInstrument<T> for AggregateObservableGauge<T> {
    fn observe(&self, measurement: T, attributes: &[KeyValue]) {
        for gauge in &self.delegates {
            gauge.observe(measurement, attributes)
        }
    }

    fn as_any(&self) -> Arc<dyn Any> {
        unreachable!()
    }
}
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
                .map(|p| {
                    let mut b = p.$name(name.clone());
                    if let Some(description) = &description {
                        b = b.with_description(description.clone());
                    }
                    if let Some(unit) = &unit {
                        b = b.with_unit(unit.clone());
                    }
                    for callback in &callback {
                        let callback = callback.clone();
                        b = b.with_callback(move |c| (*callback)(c));
                    }
                    b.try_init()
                })
                .try_collect()?;
            Ok($wrapper::new(Arc::new($implementation { delegates })))
        }
    };
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
        instruments: &[Arc<dyn Any>],
        callbacks: Box<dyn Fn(&dyn Observer) + Send + Sync>,
    ) -> opentelemetry_api::metrics::Result<Box<dyn CallbackRegistration>> {
        // The reason that this is OK is that calling observe outside of a callback is a no-op.
        // So the callback is called, an observable is updated, but only the observable associated with the correct meter will take effect

        let callback = Arc::new(callbacks);
        let mut callback_registrations = Vec::with_capacity(self.meters.len());
        for meter in &self.meters {
            let callback = callback.clone();
            // If this fails there is no recovery as some callbacks may be registered
            callback_registrations.push(meter.register_callback(instruments, move |c| callback(c))?)
        }
        Ok(Box::new(AggregatedCallbackRegistrations(
            callback_registrations,
        )))
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
