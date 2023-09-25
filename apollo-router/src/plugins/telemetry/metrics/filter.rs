use std::sync::Arc;

use buildstructor::buildstructor;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::Unit;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry::Context;
use opentelemetry::InstrumentationLibrary;
use regex::Regex;

pub(crate) struct FilterMeterProvider<T: MeterProvider> {
    delegate: T,
    deny: Option<Regex>,
    allow: Option<Regex>,
}

#[buildstructor]
impl<T: MeterProvider> FilterMeterProvider<T> {
    #[builder]
    fn new(delegate: T, deny: Option<Regex>, allow: Option<Regex>) -> Self {
        FilterMeterProvider {
            delegate,
            deny,
            allow,
        }
    }

    pub(crate) fn apollo_metrics(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .allow(
                Regex::new(
                    r"apollo\.(graphos\.cloud|router\.(operations?|config|schema|query))(\..*|$)",
                )
                .expect("regex should have been valid"),
            )
            .build()
    }

    pub(crate) fn public_metrics(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .deny(
                Regex::new(r"apollo\.router\.(config|entities)(\..*|$)")
                    .expect("regex should have been valid"),
            )
            .build()
    }
}

struct FilteredInstrumentProvider {
    noop: Meter,
    delegate: Meter,
    deny: Option<Regex>,
    allow: Option<Regex>,
}
macro_rules! filter_meter_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            name: String,
            description: Option<String>,
            unit: Option<Unit>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let mut builder = match (&self.deny, &self.allow) {
                (Some(deny), Some(allow)) if deny.is_match(&name) && !allow.is_match(&name) => {
                    self.noop.$name(name)
                }
                (Some(deny), None) if deny.is_match(&name) => self.noop.$name(name),
                (None, Some(allow)) if !allow.is_match(&name) => self.noop.$name(name),
                (_, _) => self.delegate.$name(name),
            };
            if let Some(description) = &description {
                builder = builder.with_description(description);
            }
            if let Some(unit) = &unit {
                builder = builder.with_unit(unit.clone());
            }
            builder.try_init()
        }
    };
}

impl InstrumentProvider for FilteredInstrumentProvider {
    filter_meter_fn!(u64_counter, u64, Counter);
    filter_meter_fn!(f64_counter, f64, Counter);

    filter_meter_fn!(f64_observable_counter, f64, ObservableCounter);
    filter_meter_fn!(u64_observable_counter, u64, ObservableCounter);

    filter_meter_fn!(u64_histogram, u64, Histogram);
    filter_meter_fn!(f64_histogram, f64, Histogram);
    filter_meter_fn!(i64_histogram, i64, Histogram);

    filter_meter_fn!(i64_up_down_counter, i64, UpDownCounter);
    filter_meter_fn!(f64_up_down_counter, f64, UpDownCounter);

    filter_meter_fn!(i64_observable_up_down_counter, i64, ObservableUpDownCounter);
    filter_meter_fn!(f64_observable_up_down_counter, f64, ObservableUpDownCounter);

    filter_meter_fn!(f64_observable_gauge, f64, ObservableGauge);
    filter_meter_fn!(i64_observable_gauge, i64, ObservableGauge);
    filter_meter_fn!(u64_observable_gauge, u64, ObservableGauge);

    fn register_callback(
        &self,
        callback: Box<dyn Fn(&Context) + Send + Sync>,
    ) -> opentelemetry::metrics::Result<()> {
        self.delegate.register_callback(callback)
    }
}

impl<T: MeterProvider> MeterProvider for FilterMeterProvider<T> {
    fn versioned_meter(
        &self,
        name: &'static str,
        version: Option<&'static str>,
        schema_url: Option<&'static str>,
    ) -> Meter {
        let delegate = self.delegate.versioned_meter(name, version, schema_url);
        Meter::new(
            InstrumentationLibrary::new(name, version, schema_url),
            Arc::new(FilteredInstrumentProvider {
                noop: NoopMeterProvider::new().versioned_meter(name, version, schema_url),
                delegate,
                deny: self.deny.clone(),
                allow: self.allow.clone(),
            }),
        )
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::sync::Mutex;

    use opentelemetry::metrics::noop;
    use opentelemetry::metrics::Counter;
    use opentelemetry::metrics::InstrumentProvider;
    use opentelemetry::metrics::Meter;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::metrics::Unit;
    use opentelemetry::Context;
    use opentelemetry::InstrumentationLibrary;

    use crate::plugins::telemetry::metrics::filter::FilterMeterProvider;

    #[derive(Default, Clone)]
    struct MockInstrumentProvider {
        #[allow(clippy::type_complexity)]
        counters_created: Arc<Mutex<HashSet<(String, Option<String>, Option<Unit>)>>>,
        callbacks_registered: Arc<AtomicU64>,
    }

    impl InstrumentProvider for MockInstrumentProvider {
        // We're only going to bother with testing counters and callbacks because the code is implemented as a macro and if it's right for counters it's right for everything else.
        fn u64_counter(
            &self,
            name: String,
            description: Option<String>,
            unit: Option<Unit>,
        ) -> opentelemetry::metrics::Result<Counter<u64>> {
            self.counters_created
                .lock()
                .expect("lock should not be poisoned")
                .insert((name, description, unit));
            Ok(Counter::new(Arc::new(noop::NoopSyncInstrument::new())))
        }

        fn register_callback(
            &self,
            _callback: Box<dyn Fn(&Context) + Send + Sync>,
        ) -> opentelemetry::metrics::Result<()> {
            self.callbacks_registered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default, Clone)]
    struct MockMeterProvider {
        instrument_provider: Arc<MockInstrumentProvider>,
    }

    impl MeterProvider for MockMeterProvider {
        fn versioned_meter(
            &self,
            name: &'static str,
            version: Option<&'static str>,
            schema_url: Option<&'static str>,
        ) -> Meter {
            Meter::new(
                InstrumentationLibrary::new(name, version, schema_url),
                self.instrument_provider.clone(),
            )
        }
    }

    #[test]
    fn test_apollo_metrics() {
        let delegate = MockMeterProvider::default();
        let filtered = FilterMeterProvider::apollo_metrics(delegate.clone())
            .versioned_meter("filtered", None, None);
        filtered.u64_counter("apollo.router.operations").init();
        filtered.u64_counter("apollo.router.operations.test").init();
        filtered.u64_counter("apollo.graphos.cloud.test").init();
        filtered.u64_counter("apollo.router.unknown.test").init();
        assert!(delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.operations.test".to_string(), None, None)));
        assert!(delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.operations".to_string(), None, None)));
        assert!(delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.graphos.cloud.test".to_string(), None, None)));
        assert!(!delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.unknown.test".to_string(), None, None)));
    }

    #[test]
    fn test_public_metrics() {
        let delegate = MockMeterProvider::default();
        let filtered = FilterMeterProvider::public_metrics(delegate.clone())
            .versioned_meter("filtered", None, None);
        filtered.u64_counter("apollo.router.config").init();
        filtered.u64_counter("apollo.router.config.test").init();
        filtered.u64_counter("apollo.router.entities").init();
        filtered.u64_counter("apollo.router.entities.test").init();
        assert!(!delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.config".to_string(), None, None)));
        assert!(!delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.config.test".to_string(), None, None)));
        assert!(!delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.entities".to_string(), None, None)));
        assert!(!delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&("apollo.router.entities.test".to_string(), None, None)));
    }

    #[test]
    fn test_description_and_unit() {
        let delegate = MockMeterProvider::default();
        let filtered = FilterMeterProvider::apollo_metrics(delegate.clone())
            .versioned_meter("filtered", None, None);
        filtered
            .u64_counter("apollo.router.operations")
            .with_description("desc")
            .with_unit(Unit::new("ms"))
            .init();
        assert!(delegate
            .instrument_provider
            .counters_created
            .lock()
            .unwrap()
            .contains(&(
                "apollo.router.operations".to_string(),
                Some("desc".to_string()),
                Some(Unit::new("ms"))
            )));
    }
}
