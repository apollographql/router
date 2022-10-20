use std::sync::Arc;

use itertools::Itertools;
use opentelemetry::metrics::AsyncCounter;
use opentelemetry::metrics::AsyncGauge;
use opentelemetry::metrics::AsyncUpDownCounter;
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
use opentelemetry::Context;
use opentelemetry::InstrumentationLibrary;
use opentelemetry::KeyValue;

#[derive(Clone, Default)]
pub(crate) struct AggregateMeterProvider {
    providers: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
}
impl AggregateMeterProvider {
    pub(crate) fn new(
        providers: Vec<Arc<dyn MeterProvider + Send + Sync + 'static>>,
    ) -> AggregateMeterProvider {
        AggregateMeterProvider { providers }
    }
}

impl MeterProvider for AggregateMeterProvider {
    fn versioned_meter(
        &self,
        name: &'static str,
        version: Option<&'static str>,
        schema_url: Option<&'static str>,
    ) -> Meter {
        Meter::new(
            InstrumentationLibrary::new(name, version, schema_url),
            Arc::new(AggregateInstrumentProvider {
                meters: self
                    .providers
                    .iter()
                    .map(|p| p.versioned_meter(name, version, schema_url))
                    .collect(),
            }),
        )
    }
}

pub(crate) struct AggregateInstrumentProvider {
    meters: Vec<Meter>,
}

pub(crate) struct AggregateCounter<T> {
    delegates: Vec<Counter<T>>,
}

impl<T: Copy> SyncCounter<T> for AggregateCounter<T> {
    fn add(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(cx, value, attributes)
        }
    }
}

pub(crate) struct AggregateObservableCounter<T> {
    delegates: Vec<ObservableCounter<T>>,
}

impl<T: Copy> AsyncCounter<T> for AggregateObservableCounter<T> {
    fn observe(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.observe(cx, value, attributes)
        }
    }
}

pub(crate) struct AggregateHistogram<T> {
    delegates: Vec<Histogram<T>>,
}

impl<T: Copy> SyncHistogram<T> for AggregateHistogram<T> {
    fn record(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for histogram in &self.delegates {
            histogram.record(cx, value, attributes)
        }
    }
}

pub(crate) struct AggregateUpDownCounter<T> {
    delegates: Vec<UpDownCounter<T>>,
}

impl<T: Copy> SyncUpDownCounter<T> for AggregateUpDownCounter<T> {
    fn add(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.add(cx, value, attributes)
        }
    }
}

pub(crate) struct AggregateObservableUpDownCounter<T> {
    delegates: Vec<ObservableUpDownCounter<T>>,
}

impl<T: Copy> AsyncUpDownCounter<T> for AggregateObservableUpDownCounter<T> {
    fn observe(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for counter in &self.delegates {
            counter.observe(cx, value, attributes)
        }
    }
}

pub(crate) struct AggregateObservableGauge<T> {
    delegates: Vec<ObservableGauge<T>>,
}

impl<T: Copy> AsyncGauge<T> for AggregateObservableGauge<T> {
    fn observe(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for gauge in &self.delegates {
            gauge.observe(cx, value, attributes)
        }
    }
}

macro_rules! aggregate_meter_fn {
    ($name:ident, $ty:ty, $wrapper:ident, $implementation:ident) => {
        fn $name(
            &self,
            name: String,
            description: Option<String>,
            unit: Option<Unit>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let delegates = self
                .meters
                .iter()
                .map(|p| {
                    let mut b = p.$name(name.clone());
                    if let Some(description) = &description {
                        b = b.with_description(description);
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

impl InstrumentProvider for AggregateInstrumentProvider {
    aggregate_meter_fn!(u64_counter, u64, Counter, AggregateCounter);
    aggregate_meter_fn!(f64_counter, f64, Counter, AggregateCounter);

    aggregate_meter_fn!(
        f64_observable_counter,
        f64,
        ObservableCounter,
        AggregateObservableCounter
    );
    aggregate_meter_fn!(
        u64_observable_counter,
        u64,
        ObservableCounter,
        AggregateObservableCounter
    );

    aggregate_meter_fn!(u64_histogram, u64, Histogram, AggregateHistogram);
    aggregate_meter_fn!(f64_histogram, f64, Histogram, AggregateHistogram);
    aggregate_meter_fn!(i64_histogram, i64, Histogram, AggregateHistogram);

    aggregate_meter_fn!(
        i64_up_down_counter,
        i64,
        UpDownCounter,
        AggregateUpDownCounter
    );
    aggregate_meter_fn!(
        f64_up_down_counter,
        f64,
        UpDownCounter,
        AggregateUpDownCounter
    );

    aggregate_meter_fn!(
        i64_observable_up_down_counter,
        i64,
        ObservableUpDownCounter,
        AggregateObservableUpDownCounter
    );
    aggregate_meter_fn!(
        f64_observable_up_down_counter,
        f64,
        ObservableUpDownCounter,
        AggregateObservableUpDownCounter
    );

    aggregate_meter_fn!(
        f64_observable_gauge,
        f64,
        ObservableGauge,
        AggregateObservableGauge
    );
    aggregate_meter_fn!(
        i64_observable_gauge,
        i64,
        ObservableGauge,
        AggregateObservableGauge
    );
    aggregate_meter_fn!(
        u64_observable_gauge,
        u64,
        ObservableGauge,
        AggregateObservableGauge
    );

    fn register_callback(
        &self,
        callback: Box<dyn Fn(&Context) + Send + Sync>,
    ) -> opentelemetry::metrics::Result<()> {
        // The reason that this is OK is that calling observe outside of a callback is a no-op.
        // So the callback is called, an observable is updated, but only the observable associated with the correct meter will take effect

        let callback = Arc::new(callback);
        for meter in &self.meters {
            let callback = callback.clone();
            // If this fails there is no recovery as some callbacks may be registered
            meter.register_callback(move |c| callback(c))?
        }
        Ok(())
    }
}
