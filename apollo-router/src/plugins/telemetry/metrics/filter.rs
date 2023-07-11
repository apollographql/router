use buildstructor::buildstructor;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::metrics::{Meter, MeterProvider};
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
                Regex::new(r"apollo\.router\.(operations|config)\..*")
                    .expect("regex should have been valid"),
            )
            .build()
    }

    pub(crate) fn public_metrics(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .deny(
                Regex::new(r"apollo\.router\.(operations|config)\..*")
                    .expect("regex should have been valid"),
            )
            .build()
    }
}

impl<T: MeterProvider> MeterProvider for FilterMeterProvider<T> {
    fn versioned_meter(
        &self,
        name: &'static str,
        version: Option<&'static str>,
        schema_url: Option<&'static str>,
    ) -> Meter {
        match (&self.deny, &self.allow) {
            (Some(deny), _) if !deny.is_match(name) => {
                self.delegate.versioned_meter(name, version, schema_url)
            }
            (_, Some(allow)) if allow.is_match(name) => {
                self.delegate.versioned_meter(name, version, schema_url)
            }
            (_, _) => NoopMeterProvider::default().versioned_meter(name, version, schema_url),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::plugins::telemetry::metrics::filter::FilterMeterProvider;
    use opentelemetry::metrics::noop::NoopMeterProvider;
    use opentelemetry::metrics::{Meter, MeterProvider};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct MockMeterProvider {
        meters: Arc<Mutex<HashSet<String>>>,
    }

    impl MeterProvider for MockMeterProvider {
        fn versioned_meter(
            &self,
            name: &'static str,
            version: Option<&'static str>,
            schema_url: Option<&'static str>,
        ) -> Meter {
            self.meters
                .lock()
                .expect("mutex poisoned")
                .insert(name.to_string());
            NoopMeterProvider::new().versioned_meter(name, version, schema_url)
        }
    }

    #[test]
    fn test_apollo_metrics() {
        let delegate = MockMeterProvider::default();
        let filtered = FilterMeterProvider::apollo_metrics(delegate.clone());
        filtered.versioned_meter("apollo.router.operations.test", None, None);
        filtered.versioned_meter("apollo.router.unknown.test", None, None);
        assert!(delegate
            .meters
            .lock()
            .unwrap()
            .contains("apollo.router.operations.test"));
        assert!(!delegate
            .meters
            .lock()
            .unwrap()
            .contains("apollo.router.unknown.test"));
    }

    #[test]
    fn test_filter() {
        let delegate = MockMeterProvider::default();
        let filtered = FilterMeterProvider::public_metrics(delegate.clone());
        filtered.versioned_meter("apollo.router.operations.test", None, None);
        filtered.versioned_meter("apollo.router.unknown.test", None, None);
        assert!(!delegate
            .meters
            .lock()
            .unwrap()
            .contains("apollo.router.operations.test"));
        assert!(delegate
            .meters
            .lock()
            .unwrap()
            .contains("apollo.router.unknown.test"));
    }
}
