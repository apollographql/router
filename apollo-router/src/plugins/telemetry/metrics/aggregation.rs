use itertools::Itertools;
use opentelemetry::metrics::{
    Counter, InstrumentProvider, Meter, MeterProvider, SyncCounter, Unit,
};
use opentelemetry::{Context, InstrumentationLibrary, KeyValue};
use std::sync::Arc;

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

pub(crate) struct AggregateSyncCounter<T> {
    counters: Vec<Counter<T>>,
}

impl<T: Copy> SyncCounter<T> for AggregateSyncCounter<T> {
    fn add(&self, cx: &Context, value: T, attributes: &[KeyValue]) {
        for counter in &self.counters {
            counter.add(cx, value, attributes)
        }
    }
}

impl InstrumentProvider for AggregateInstrumentProvider {
    fn f64_counter(
        &self,
        name: String,
        description: Option<String>,
        unit: Option<Unit>,
    ) -> opentelemetry::metrics::Result<Counter<f64>> {
        let counters = self
            .meters
            .iter()
            .map(|p| {
                let mut b = p.f64_counter(name.clone());
                if let Some(description) = &description {
                    b = b.with_description(description);
                }
                if let Some(unit) = &unit {
                    b = b.with_unit(unit.clone());
                }
                b.try_init()
            })
            .try_collect()?;
        Ok(Counter::new(Arc::new(AggregateSyncCounter { counters })))
    }

    fn register_callback(
        &self,
        callback: Box<dyn Fn(&Context) + Send + Sync>,
    ) -> opentelemetry::metrics::Result<()> {
        let callback = Arc::new(callback);
        for meter in &self.meters {
            let callback = callback.clone();
            // If this fails there is no recovery as some callbacks may be registered
            meter.register_callback(move |c| callback(c))?
        }
        Ok(())
    }
}
