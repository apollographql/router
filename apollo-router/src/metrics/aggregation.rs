use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;
use std::time::Duration;

use derive_more::From;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry::metrics::AsyncInstrument;
use opentelemetry::metrics::AsyncInstrumentBuilder;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::HistogramBuilder;
use opentelemetry::metrics::InstrumentBuilder;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::SyncInstrument;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use parking_lot::Mutex;
use strum::Display;
use strum::EnumCount;
use strum::EnumIter;
use strum::FromRepr;
use strum::IntoEnumIterator;

use super::NoopInstrumentProvider;
use crate::metrics::filter::FilterMeterProvider;

// This meter provider enables us to combine multiple meter providers. The reasons we need this are:
// 1. Prometheus meters are special. To dispose a meter is to dispose the entire registry. This means we need to make a best effort to keep them around.
// 2. To implement filtering we use a view. However this must be set during build of the meter provider, thus we need separate ones for Apollo and general metrics.
// Unlike the regular meter provider this implementation will return an existing meter if one has been created already rather than a new one.
// This is within the spec: https://opentelemetry.io/docs/specs/otel/metrics/api/#get-a-meter
// `Meters are identified by name, version, and schema_url fields. When more than one Meter of the same name, version, and schema_url is created, it is unspecified whether or under which conditions the same or different Meter instances are returned. It is a user error to create Meters with different attributes but the same identity.`

#[derive(
    Hash, Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug, EnumCount, EnumIter, Display, FromRepr,
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
        // All providers start as noop to avoid OTel SDK errors.
        // Real providers are set via set() during configuration.
        AggregateMeterProvider {
            inner: Arc::new(Mutex::new(Some(Inner::default()))),
        }
    }
}

pub(crate) struct Inner {
    providers: Vec<(FilterMeterProvider, HashMap<MeterId, Meter>)>,
    registered_instruments: Vec<InstrumentWrapper>,
    /// Shared registries for observable instruments - tracks registrations per provider
    observable_registries: Arc<SharedObservableRegistries>,
}

impl Default for Inner {
    fn default() -> Self {
        Inner {
            // Initialize with noop providers to avoid OTel SDK errors during startup.
            // Real providers are set via AggregateMeterProvider::set() during configuration.
            providers: (0..MeterProviderType::COUNT)
                .map(|_| (FilterMeterProvider::noop(), HashMap::new()))
                .collect(),
            registered_instruments: Vec::new(),
            observable_registries: Arc::new(SharedObservableRegistries::new(
                MeterProviderType::COUNT,
            )),
        }
    }
}

// HACK(@goto-bus-stop): see https://github.com/apollographql/router/pull/8976
// _in tests_, we store the AggregateMeterProvider in a thread local.
// During Drop, the otel meter providers may log using `tracing`. This also uses a thread local.
// When the thread exits, such locals are dropped in unspecified order: `tracing` may be dropped
// _before_ the AggregateMeterProvider. In that case, otel would try to log and `tracing` may
// panic internally.
//
// This implementation works around that issue by manually dropping the otel meter providers while
// _disabling_ tracing logs altogether.
//
// This assumes that in _normal_ router operation, we always call `shutdown()` explicitly, else we
// would lose trace events.
impl Drop for Inner {
    fn drop(&mut self) {
        let noop = tracing::subscriber::NoSubscriber::new();
        tracing::subscriber::with_default(noop, || {
            self.providers.clear();
        });
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

        // Clear observable registrations for this provider so new gauges will re-register
        inner
            .observable_registries
            .clear_provider(meter_provider_type);

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
    pub(crate) fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        /// Prefix internal failure error message with "[providername]".
        /// Returns the original error if it is not an internal failure or if the index is invalid.
        fn tag_error_with_provider_type(err: OTelSdkError, index: usize) -> OTelSdkError {
            let OTelSdkError::InternalFailure(message) = &err else {
                return err;
            };

            let Ok(ty) = u8::try_from(index) else {
                return err;
            };
            let Some(ty) = MeterProviderType::from_repr(ty) else {
                return err;
            };

            OTelSdkError::InternalFailure(format!("[{ty}] {message}"))
        }

        // Make sure that we don't deadlock by dropping the mutex guard before actual shutdown happens
        // This means that if we have any misbehaving code that tries to access the meter provider during shutdown, e.g. for export metrics
        // then we don't get stuck on the mutex.
        // For instance the apollo exporters have in the past had metrics for exporting, as
        // they shut down they try to increment a metric which causes a new meter to be created.
        // However, if we have not released the guard then we deadlock.
        let mut guard = self.inner.lock();
        let old = guard.take();
        drop(guard);

        let Some(inner) = old else { return Ok(()) };

        let mut result = Ok(());
        for (index, (provider, _)) in inner.providers.iter().enumerate() {
            let Err(err) = provider.shutdown_with_timeout(timeout) else {
                continue;
            };

            let err = tag_error_with_provider_type(err, index);

            // Report errors as, in order of precedence:
            // - combined internal failures tagged with provider type
            // - or timeout if one of them timed out
            // - or AlreadyShutdown if one of them was already shut down
            // - or nothing
            // "Less important" errors get silenced in favour of the "more important" ones. Scare
            // quotes due to inherent subjectivity.
            result = match (result, err) {
                // Report all stacking internal failures
                (
                    Err(OTelSdkError::InternalFailure(old_error)),
                    OTelSdkError::InternalFailure(new_error),
                ) => Err(OTelSdkError::InternalFailure(format!(
                    "{old_error}\n{new_error}"
                ))),
                // If we already had an internal failure, keep that
                (result @ Err(OTelSdkError::InternalFailure(_)), _) => result,
                // If we now encountered an internal failure, keep the new error
                (_, err @ OTelSdkError::InternalFailure(_)) => Err(err),
                // If we already had an error, keep that
                (result @ Err(_), _) => result,
                // If we did not yet have an error, stash the new error
                (Ok(_), err) => Err(err),
            };
        }
        Ok(())
    }

    /// Shutdown MUST be called from a blocking thread.
    pub(crate) fn shutdown(&self) -> OTelSdkResult {
        self.shutdown_with_timeout(Duration::from_secs(5))
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
    pub(crate) fn meter(&mut self, name: impl Into<Cow<'static, str>>) -> Meter {
        let scope = InstrumentationScope::builder(name).build();
        self.meter_with_scope(scope)
    }
    pub(crate) fn meter_with_scope(&mut self, scope: InstrumentationScope) -> Meter {
        let name: Cow<'static, str> = Cow::Owned(scope.name().to_string());
        let version: Option<Cow<'static, str>> = scope.version().map(|v| Cow::Owned(v.to_string()));
        let schema_url: Option<Cow<'static, str>> =
            scope.schema_url().map(|v| Cow::Owned(v.to_string()));
        let mut meters = Vec::with_capacity(self.providers.len());

        for (provider, existing_meters) in &mut self.providers {
            meters.push(
                existing_meters
                    .entry(MeterId {
                        name: name.clone(),
                        version: version.clone(),
                        schema_url: schema_url.clone(),
                    })
                    .or_insert_with(|| provider.meter_with_scope(scope.clone()))
                    .clone(),
            );
        }

        Meter::new(Arc::new(AggregateInstrumentProvider {
            meters,
            registries: Arc::clone(&self.observable_registries),
        }))
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
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        let mut inner = self.inner.lock();
        if let Some(inner) = inner.as_mut() {
            inner.meter_with_scope(scope)
        } else {
            // The meter was used after shutdown. Default to Noop since the instrument cannot actually be used
            Meter::new(Arc::new(NoopInstrumentProvider))
        }
    }
}

pub(crate) struct AggregateInstrumentProvider {
    meters: Vec<Meter>,
    /// Shared registries for observable instruments (owned by Inner)
    registries: Arc<SharedObservableRegistries>,
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

/// Type alias for observable callbacks
type ObservableCallback<T> = Arc<dyn Fn(&dyn AsyncInstrument<T>) + Send + Sync>;

/// Registry for observable instrument callbacks.
///
/// In OTel 0.31+, observable instrument types like `ObservableGauge<T>` are just
/// `PhantomData<T>` - dropping them does nothing. Callbacks are registered with
/// the SDK at build time and live until provider shutdown.
///
/// This registry provides proper lifecycle management:
/// - Multiple callbacks can be registered per instrument_name (e.g., different caches
///   using the same gauge name but different attribute values)
/// - One OTel instrument per (meter_provider_type, instrument_name) is registered lazily
/// - The consolidated callback invokes ALL registered user callbacks for that instrument
/// - When a provider is replaced, its registrations are cleared so new gauges re-register
struct ObservableCallbackRegistry<T: Send + Sync + 'static> {
    /// instrument_name -> list of callbacks
    /// Multiple callbacks can exist for the same instrument name when different components
    /// (e.g., query planner cache, APQ cache) use the same gauge name with different attributes
    callbacks: Mutex<HashMap<String, Vec<ObservableCallback<T>>>>,
    /// Tracks which (meter_provider_type, instrument_name) pairs have been registered with OTel SDK
    registered: Mutex<HashSet<(MeterProviderType, String)>>,
}

impl<T: Send + Sync + 'static> ObservableCallbackRegistry<T> {
    fn new() -> Self {
        Self {
            callbacks: Mutex::new(HashMap::new()),
            registered: Mutex::new(HashSet::new()),
        }
    }

    /// Register a callback for an instrument name.
    /// Multiple callbacks can be registered for the same instrument name.
    /// This supports scenarios like multiple caches using the same gauge name
    /// but with different attribute values (e.g., `kind="query planner"` vs `kind="apq"`).
    fn register_callback(&self, instrument_name: &str, callback: ObservableCallback<T>) {
        let mut callbacks = self.callbacks.lock();
        callbacks
            .entry(instrument_name.to_string())
            .or_default()
            .push(callback);
    }

    /// Invoke all callbacks for an instrument name.
    fn invoke_callback(&self, instrument_name: &str, observer: &dyn AsyncInstrument<T>) {
        let callbacks = self.callbacks.lock();
        if let Some(callback_list) = callbacks.get(instrument_name) {
            for callback in callback_list {
                callback(observer);
            }
        }
    }

    /// Register an instrument for a provider if not already registered.
    /// Returns `true` if newly registered, `false` if already registered.
    fn try_register_for_provider(
        &self,
        meter_provider_type: MeterProviderType,
        instrument_name: String,
    ) -> bool {
        self.registered
            .lock()
            .insert((meter_provider_type, instrument_name))
    }

    /// Clear registrations for a specific provider (called when provider is replaced)
    fn clear_provider_registrations(&self, meter_provider_type: MeterProviderType) {
        let mut registered = self.registered.lock();
        registered.retain(|(provider_type, _)| *provider_type != meter_provider_type);
    }

    /// Clear all callbacks (called during reload when services will be recreated)
    fn clear_callbacks(&self) {
        self.callbacks.lock().clear();
    }
}

/// Shared registries for all observable instrument types.
/// This is stored at the `Inner` level and shared across all meters.
pub(crate) struct SharedObservableRegistries {
    u64_gauge: ObservableCallbackRegistry<u64>,
    i64_gauge: ObservableCallbackRegistry<i64>,
    f64_gauge: ObservableCallbackRegistry<f64>,
    u64_counter: ObservableCallbackRegistry<u64>,
    f64_counter: ObservableCallbackRegistry<f64>,
    i64_up_down_counter: ObservableCallbackRegistry<i64>,
    f64_up_down_counter: ObservableCallbackRegistry<f64>,
}

impl SharedObservableRegistries {
    fn new(_num_providers: usize) -> Self {
        Self {
            u64_gauge: ObservableCallbackRegistry::new(),
            i64_gauge: ObservableCallbackRegistry::new(),
            f64_gauge: ObservableCallbackRegistry::new(),
            u64_counter: ObservableCallbackRegistry::new(),
            f64_counter: ObservableCallbackRegistry::new(),
            i64_up_down_counter: ObservableCallbackRegistry::new(),
            f64_up_down_counter: ObservableCallbackRegistry::new(),
        }
    }

    /// Clear registrations for a specific provider and all callbacks.
    ///
    /// Called when a provider is replaced. We clear:
    /// 1. Provider registrations - so new gauges will re-register with the new provider
    /// 2. All callbacks - because services will be recreated and re-register their callbacks
    ///
    /// This is safe because when any provider is replaced, the entire service graph is
    /// recreated, so all gauges will be recreated and add fresh callbacks.
    fn clear_provider(&self, meter_provider_type: MeterProviderType) {
        // Clear registrations for this provider so new gauges will register with it
        self.u64_gauge
            .clear_provider_registrations(meter_provider_type);
        self.i64_gauge
            .clear_provider_registrations(meter_provider_type);
        self.f64_gauge
            .clear_provider_registrations(meter_provider_type);
        self.u64_counter
            .clear_provider_registrations(meter_provider_type);
        self.f64_counter
            .clear_provider_registrations(meter_provider_type);
        self.i64_up_down_counter
            .clear_provider_registrations(meter_provider_type);
        self.f64_up_down_counter
            .clear_provider_registrations(meter_provider_type);

        // Clear all callbacks - services will be recreated and re-register them
        self.u64_gauge.clear_callbacks();
        self.i64_gauge.clear_callbacks();
        self.f64_gauge.clear_callbacks();
        self.u64_counter.clear_callbacks();
        self.f64_counter.clear_callbacks();
        self.i64_up_down_counter.clear_callbacks();
        self.f64_up_down_counter.clear_callbacks();
    }
}

// Macro for sync instruments (Counter, UpDownCounter, Gauge)
macro_rules! aggregate_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(&self, builder: InstrumentBuilder<'_, $wrapper<$ty>>) -> $wrapper<$ty> {
            let delegates: Vec<$wrapper<$ty>> = self
                .meters
                .iter()
                .map(|meter| {
                    let mut b = meter.$name(builder.name.clone());
                    if let Some(description) = &builder.description {
                        b = b.with_description(description.clone());
                    }
                    if let Some(unit) = &builder.unit {
                        b = b.with_unit(unit.clone());
                    }
                    b.build()
                })
                .collect();
            $wrapper::new(Arc::new($implementation { delegates }))
        }
    };
}

// Macro for histogram instruments
macro_rules! aggregate_histogram_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(&self, builder: HistogramBuilder<'_, $wrapper<$ty>>) -> $wrapper<$ty> {
            let delegates: Vec<$wrapper<$ty>> = self
                .meters
                .iter()
                .map(|meter| {
                    let mut b = meter.$name(builder.name.clone());
                    if let Some(description) = &builder.description {
                        b = b.with_description(description.clone());
                    }
                    if let Some(unit) = &builder.unit {
                        b = b.with_unit(unit.clone());
                    }
                    // Copy boundaries if set
                    if let Some(boundaries) = &builder.boundaries {
                        b = b.with_boundaries(boundaries.clone());
                    }
                    b.build()
                })
                .collect();
            $wrapper::new(Arc::new($implementation { delegates }))
        }
    };
}

/// Macro for observable gauge instruments using the registry pattern.
///
/// In OTel 0.31+, observable instruments work through callbacks registered at build time.
/// The SDK's `ObservableGauge<T>` is just `PhantomData<T>` - dropping it does nothing.
///
/// This macro implements a registry pattern:
/// 1. User callbacks are stored in a shared registry indexed by gauge name
/// 2. One OTel gauge per (provider, name) is registered lazily with a consolidated callback
/// 3. The consolidated callback invokes all registered user callbacks
/// 4. When a provider is replaced, its registrations are cleared so new gauges re-register
macro_rules! aggregate_observable_gauge_fn {
    ($name:ident, $ty:ty, $registry:ident) => {
        fn $name(
            &self,
            builder: AsyncInstrumentBuilder<'_, ObservableGauge<$ty>, $ty>,
        ) -> ObservableGauge<$ty> {
            let gauge_name = builder.name.to_string();
            let description = builder.description.as_ref().map(|s| s.to_string());
            let unit = builder.unit.as_ref().map(|s| s.to_string());

            // Wrap callbacks in Arc so they can be shared
            let shared_callbacks: Vec<ObservableCallback<$ty>> =
                builder.callbacks.into_iter().map(Arc::from).collect();

            // If no callbacks, just return noop (matches OTel behavior)
            if shared_callbacks.is_empty() {
                return ObservableGauge::new();
            }

            // Register callbacks in the shared registry
            for callback in shared_callbacks {
                self.registries
                    .$registry
                    .register_callback(&gauge_name, callback);
            }

            // Register with each delegate meter that hasn't been registered yet
            for (meter, meter_provider_type) in self.meters.iter().zip(MeterProviderType::iter()) {
                if !self
                    .registries
                    .$registry
                    .try_register_for_provider(meter_provider_type, gauge_name.clone())
                {
                    continue;
                }

                let mut b = meter.$name(gauge_name.clone());
                if let Some(desc) = &description {
                    b = b.with_description(desc.clone());
                }
                if let Some(u) = &unit {
                    b = b.with_unit(u.clone());
                }
                // Consolidated callback that invokes all registered callbacks
                let registry = Arc::clone(&self.registries);
                let name = gauge_name.clone();
                b = b.with_callback(move |observer| {
                    registry.$registry.invoke_callback(&name, observer);
                });
                // Build registers the callback with OTel SDK
                // The returned ObservableGauge is PhantomData, no need to store it
                let _ = b.build();
            }

            ObservableGauge::new()
        }
    };
}

/// Macro for observable counter/up-down-counter instruments using the registry pattern.
macro_rules! aggregate_observable_counter_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $registry:ident) => {
        fn $name(&self, builder: AsyncInstrumentBuilder<'_, $wrapper<$ty>, $ty>) -> $wrapper<$ty> {
            let instrument_name = builder.name.to_string();
            let description = builder.description.as_ref().map(|s| s.to_string());
            let unit = builder.unit.as_ref().map(|s| s.to_string());

            // Wrap callbacks in Arc so they can be shared
            let shared_callbacks: Vec<ObservableCallback<$ty>> =
                builder.callbacks.into_iter().map(Arc::from).collect();

            // If no callbacks, just return noop (matches OTel behavior)
            if shared_callbacks.is_empty() {
                return $wrapper::new();
            }

            // Register callbacks in the shared registry
            for callback in shared_callbacks {
                self.registries
                    .$registry
                    .register_callback(&instrument_name, callback);
            }

            // Register with each delegate meter that hasn't been registered yet
            for (meter, meter_provider_type) in self.meters.iter().zip(MeterProviderType::iter()) {
                if !self
                    .registries
                    .$registry
                    .try_register_for_provider(meter_provider_type, instrument_name.clone())
                {
                    continue;
                }

                let mut b = meter.$name(instrument_name.clone());
                if let Some(desc) = &description {
                    b = b.with_description(desc.clone());
                }
                if let Some(u) = &unit {
                    b = b.with_unit(u.clone());
                }
                // Consolidated callback that invokes all registered callbacks
                let registry = Arc::clone(&self.registries);
                let name = instrument_name.clone();
                b = b.with_callback(move |observer| {
                    registry.$registry.invoke_callback(&name, observer);
                });
                // Build registers the callback with OTel SDK
                // The returned type is PhantomData, no need to store it
                let _ = b.build();
            }

            $wrapper::new()
        }
    };
}

impl InstrumentProvider for AggregateInstrumentProvider {
    aggregate_instrument_fn!(u64_counter, u64, Counter, AggregateCounter);
    aggregate_instrument_fn!(f64_counter, f64, Counter, AggregateCounter);

    aggregate_observable_counter_fn!(f64_observable_counter, f64, ObservableCounter, f64_counter);
    aggregate_observable_counter_fn!(u64_observable_counter, u64, ObservableCounter, u64_counter);

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

    aggregate_observable_counter_fn!(
        i64_observable_up_down_counter,
        i64,
        ObservableUpDownCounter,
        i64_up_down_counter
    );
    aggregate_observable_counter_fn!(
        f64_observable_up_down_counter,
        f64,
        ObservableUpDownCounter,
        f64_up_down_counter
    );

    aggregate_observable_gauge_fn!(f64_observable_gauge, f64, f64_gauge);
    aggregate_observable_gauge_fn!(i64_observable_gauge, i64, i64_gauge);
    aggregate_observable_gauge_fn!(u64_observable_gauge, u64, u64_gauge);
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::Weak;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::InstrumentKind;
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::Pipeline;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::AggregatedMetrics;
    use opentelemetry_sdk::metrics::data::MetricData;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use opentelemetry_sdk::runtime;

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

        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.0.shutdown_with_timeout(timeout)
        }

        fn temporality(&self, kind: InstrumentKind) -> Temporality {
            self.0.temporality(kind)
        }
    }

    #[test]
    fn test_i64_gauge_callback_invocation() {
        // In OTel 0.31+, observable instrument callbacks are registered with the SDK
        // and persist until the meter provider is shut down. Dropping the returned
        // ObservableGauge marker type does NOT unregister the callback.
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
        let _gauge = meter
            .i64_observable_gauge("test")
            .with_callback(move |i| {
                let count =
                    callback_observe_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                i.observe(count + 1, &[])
            })
            .build();

        let mut result = ResourceMetrics::default();

        // Fetching twice will call the observer twice
        reader
            .collect(&mut result)
            .expect("metrics must be collected");
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(get_gauge_value(&result), 2);
        assert_eq!(observe_counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn test_i64_gauge_multiple_callbacks() {
        // In OTel 0.31+, multiple observable gauges with the same name can coexist
        // and their callbacks are all invoked during collection.
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
        let _gauge1 = meter
            .i64_observable_gauge("test")
            .with_callback(move |i| {
                let count =
                    callback_observe_counter1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                i.observe(count + 1, &[])
            })
            .build();

        let mut result = ResourceMetrics::default();

        // Fetching metrics will call the observer
        reader
            .collect(&mut result)
            .expect("metrics must be collected");

        assert_eq!(get_gauge_value(&result), 1);
        assert_eq!(observe_counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn get_gauge_value(result: &ResourceMetrics) -> i64 {
        let scope_metrics: Vec<_> = result.scope_metrics().collect();
        assert_eq!(scope_metrics.len(), 1);
        let metrics: Vec<_> = scope_metrics.first().unwrap().metrics().collect();
        assert_eq!(metrics.len(), 1);
        let metric = metrics.first().unwrap();
        if let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() {
            assert_eq!(gauge.data_points().count(), 1);
            gauge.data_points().next().unwrap().value()
        } else {
            panic!("Expected i64 gauge")
        }
    }

    #[test]
    fn test_otel_default_meter_provider() {
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
            .meter("test")
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
        fn export(
            &self,
            _metrics: &ResourceMetrics,
        ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
            self.count();
            std::future::ready(Ok(()))
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

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    impl TestExporter {
        fn count(&self) {
            let counter = self
                .meter_provider
                .meter("test")
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
        meter_provider.shutdown().unwrap();

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
        PeriodicReader::builder(
            TestExporter {
                meter_provider: meter_provider.clone(),
                shutdown: shutdown.clone(),
            },
            runtime::Tokio,
        )
        .with_interval(Duration::from_millis(10))
        .build()
    }
}
