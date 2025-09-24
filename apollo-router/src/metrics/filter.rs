use std::any::Any;
use std::borrow::Cow;
use std::sync::Arc;

use buildstructor::buildstructor;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Callback;
use opentelemetry::metrics::CallbackRegistration;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider as OtelMeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::Observer;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry::metrics::noop::NoopMeterProvider;
use regex::Regex;

#[derive(Clone)]
pub(crate) enum MeterProvider {
    Regular(opentelemetry_sdk::metrics::SdkMeterProvider),
    Global(opentelemetry::global::GlobalMeterProvider),
}

impl MeterProvider {
    fn versioned_meter(
        &self,
        name: impl Into<Cow<'static, str>>,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        match &self {
            MeterProvider::Regular(provider) => {
                provider.versioned_meter(name, version, schema_url, attributes)
            }
            MeterProvider::Global(provider) => {
                provider.versioned_meter(name, version, schema_url, attributes)
            }
        }
    }
    fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
        match self {
            MeterProvider::Regular(provider) => provider.shutdown(),
            MeterProvider::Global(_provider) => Ok(()),
        }
    }

    fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
        match self {
            MeterProvider::Regular(provider) => provider.force_flush(),
            MeterProvider::Global(_provider) => Ok(()),
        }
    }
}

impl From<opentelemetry_sdk::metrics::SdkMeterProvider> for MeterProvider {
    fn from(provider: opentelemetry_sdk::metrics::SdkMeterProvider) -> Self {
        MeterProvider::Regular(provider)
    }
}

impl From<opentelemetry::global::GlobalMeterProvider> for MeterProvider {
    fn from(provider: opentelemetry::global::GlobalMeterProvider) -> Self {
        MeterProvider::Global(provider)
    }
}

#[derive(Clone)]
pub(crate) struct FilterMeterProvider {
    delegate: MeterProvider,
    deny: Option<Regex>,
    allow: Option<Regex>,
}

#[buildstructor]
impl FilterMeterProvider {
    #[builder]
    fn new<T: Into<MeterProvider>>(delegate: T, deny: Option<Regex>, allow: Option<Regex>) -> Self {
        FilterMeterProvider {
            delegate: delegate.into(),
            deny,
            allow,
        }
    }

    fn get_private_realtime_regex() -> Regex {
        Regex::new(r"apollo\.router\.operations\.(?:error|fetch\.duration)")
            .expect("regex should have been valid")
    }

    pub(crate) fn apollo_realtime<T: Into<MeterProvider>>(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .allow(Self::get_private_realtime_regex().clone())
            .build()
    }

    pub(crate) fn apollo<T: Into<MeterProvider>>(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .allow(
                Regex::new(
                  r"apollo\.(graphos\.cloud|router\.(operations?|lifecycle|config|schema|query|query_planning|telemetry|instance|graphql_error))(\..*|$)|apollo_router_uplink_fetch_count_total|apollo_router_uplink_fetch_duration_seconds",
                )
                .expect("regex should have been valid"),
            )
            .deny(Self::get_private_realtime_regex().clone())
            .build()
    }

    pub(crate) fn public<T: Into<MeterProvider>>(delegate: T) -> Self {
        FilterMeterProvider::builder()
            .delegate(delegate)
            .deny(
                Regex::new(r"apollo\.router\.(config|entities|instance|operations\.(connectors|fetch|request_size|response_size|error)|schema\.connectors)(\..*|$)")
                    .expect("regex should have been valid"),
            )
            .build()
    }

    #[cfg(test)]
    pub(crate) fn all<T: Into<MeterProvider>>(delegate: T) -> Self {
        FilterMeterProvider::builder().delegate(delegate).build()
    }

    pub(crate) fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
        self.delegate.shutdown()
    }

    #[allow(dead_code)]
    pub(crate) fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
        self.delegate.force_flush()
    }
}

struct FilteredInstrumentProvider {
    delegate: Meter,
    noop: Meter,
    deny: Option<Regex>,
    allow: Option<Regex>,
}

macro_rules! filter_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            name: Cow<'static, str>,
            description: Option<Cow<'static, str>>,
            unit: Option<Cow<'static, str>>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let mut builder = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&name) => self.noop.$name(name),
                (_, Some(allow)) if !allow.is_match(&name) => self.noop.$name(name),
                (_, _) => self.delegate.$name(name),
            };
            if let Some(description) = &description {
                builder = builder.with_description(description.clone())
            }
            if let Some(unit) = &unit {
                builder = builder.with_unit(unit.clone());
            }
            builder.try_init()
        }
    };
}

macro_rules! filter_observable_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            name: Cow<'static, str>,
            description: Option<Cow<'static, str>>,
            unit: Option<Cow<'static, str>>,
            callback: Vec<Callback<$ty>>,
        ) -> opentelemetry::metrics::Result<$wrapper<$ty>> {
            let mut builder = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&name) => self.noop.$name(name),
                (_, Some(allow)) if !allow.is_match(&name) => self.noop.$name(name),
                (_, _) => self.delegate.$name(name),
            };
            if let Some(description) = &description {
                builder = builder.with_description(description.clone());
            }
            if let Some(unit) = &unit {
                builder = builder.with_unit(unit.clone());
            }

            for callback in callback {
                builder = builder.with_callback(callback);
            }

            builder.try_init()
        }
    };
}

impl InstrumentProvider for FilteredInstrumentProvider {
    filter_instrument_fn!(u64_counter, u64, Counter);
    filter_instrument_fn!(f64_counter, f64, Counter);

    filter_instrument_fn!(u64_gauge, u64, Gauge);
    filter_instrument_fn!(i64_gauge, i64, Gauge);
    filter_instrument_fn!(f64_gauge, f64, Gauge);

    filter_observable_instrument_fn!(f64_observable_counter, f64, ObservableCounter);
    filter_observable_instrument_fn!(u64_observable_counter, u64, ObservableCounter);

    filter_instrument_fn!(u64_histogram, u64, Histogram);
    filter_instrument_fn!(f64_histogram, f64, Histogram);

    filter_instrument_fn!(i64_up_down_counter, i64, UpDownCounter);
    filter_instrument_fn!(f64_up_down_counter, f64, UpDownCounter);

    filter_observable_instrument_fn!(i64_observable_up_down_counter, i64, ObservableUpDownCounter);
    filter_observable_instrument_fn!(f64_observable_up_down_counter, f64, ObservableUpDownCounter);

    filter_observable_instrument_fn!(f64_observable_gauge, f64, ObservableGauge);
    filter_observable_instrument_fn!(i64_observable_gauge, i64, ObservableGauge);
    filter_observable_instrument_fn!(u64_observable_gauge, u64, ObservableGauge);

    fn register_callback(
        &self,
        instruments: &[Arc<dyn Any>],
        callbacks: Box<dyn Fn(&dyn Observer) + Send + Sync>,
    ) -> opentelemetry::metrics::Result<Box<dyn CallbackRegistration>> {
        self.delegate.register_callback(instruments, callbacks)
    }
}

impl opentelemetry::metrics::MeterProvider for FilterMeterProvider {
    fn versioned_meter(
        &self,
        name: impl Into<Cow<'static, str>>,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        Meter::new(Arc::new(FilteredInstrumentProvider {
            noop: NoopMeterProvider::default().meter(""),
            delegate: self
                .delegate
                .versioned_meter(name, version, schema_url, attributes),
            deny: self.deny.clone(),
            allow: self.allow.clone(),
        }))
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::global::GlobalMeterProvider;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::PeriodicReader;
    use opentelemetry_sdk::runtime;
    use opentelemetry_sdk::testing::metrics::InMemoryMetricsExporter;

    use crate::metrics::filter::FilterMeterProvider;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_metrics() {
        let exporter = InMemoryMetricsExporter::default();
        let meter_provider = FilterMeterProvider::apollo(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                .build(),
        );
        let filtered = meter_provider.versioned_meter("filtered", "".into(), "".into(), None);
        // Matches allow
        filtered
            .u64_counter("apollo.router.operations")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.test")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.graphos.cloud.test")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.query_planning.test")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.lifecycle.api_schema")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.connectors")
            .init()
            .add(1, &[]);
        filtered
            .u64_observable_gauge("apollo.router.schema.connectors")
            .with_callback(move |observer| observer.observe(1, &[]))
            .init();

        // Mismatches allow
        filtered
            .u64_counter("apollo.router.unknown.test")
            .init()
            .add(1, &[]);

        // Matches deny
        filtered
            .u64_counter("apollo.router.operations.error")
            .init()
            .add(1, &[]);

        meter_provider.force_flush().unwrap();

        let metrics: Vec<_> = exporter
            .get_finished_metrics()
            .unwrap()
            .into_iter()
            .flat_map(|m| m.scope_metrics.into_iter())
            .flat_map(|m| m.metrics)
            .collect();

        // Matches allow
        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.test")
        );

        assert!(metrics.iter().any(|m| m.name == "apollo.router.operations"));

        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.graphos.cloud.test")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.router.lifecycle.api_schema")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.connectors")
        );
        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.router.schema.connectors")
        );

        // Mismatches allow
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.unknown.test")
        );

        // Matches deny
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.error")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_description_and_unit() {
        let exporter = InMemoryMetricsExporter::default();
        let meter_provider = FilterMeterProvider::apollo(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                .build(),
        );
        let filtered = meter_provider.versioned_meter("filtered", "".into(), "".into(), None);
        filtered
            .u64_counter("apollo.router.operations")
            .with_description("desc")
            .with_unit("ms")
            .init()
            .add(1, &[]);
        meter_provider.force_flush().unwrap();

        let metrics: Vec<_> = exporter
            .get_finished_metrics()
            .unwrap()
            .into_iter()
            .flat_map(|m| m.scope_metrics.into_iter())
            .flat_map(|m| m.metrics)
            .collect();
        assert!(metrics.iter().any(|m| m.name == "apollo.router.operations"
            && m.description == "desc"
            && m.unit == "ms"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_public_metrics_using_meter_provider() {
        let exporter = InMemoryMetricsExporter::default();
        test_public_metrics(
            exporter.clone(),
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                .build(),
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_public_metrics_using_global_meter_provider() {
        let exporter = InMemoryMetricsExporter::default();

        test_public_metrics(
            exporter.clone(),
            GlobalMeterProvider::new(
                MeterProviderBuilder::default()
                    .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                    .build(),
            ),
        )
        .await;
    }
    async fn test_public_metrics<T: Into<super::MeterProvider>>(
        exporter: InMemoryMetricsExporter,
        meter_provider: T,
    ) {
        let meter_provider = FilterMeterProvider::public(meter_provider);
        let filtered = meter_provider.versioned_meter("filtered", "".into(), "".into(), None);
        filtered
            .u64_counter("apollo.router.config")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.config.test")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.entities")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.entities.test")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.connectors")
            .init()
            .add(1, &[]);
        filtered
            .u64_observable_gauge("apollo.router.schema.connectors")
            .with_callback(move |observer| observer.observe(1, &[]))
            .init();
        meter_provider.force_flush().unwrap();

        let metrics: Vec<_> = exporter
            .get_finished_metrics()
            .unwrap()
            .into_iter()
            .flat_map(|m| m.scope_metrics.into_iter())
            .flat_map(|m| m.metrics)
            .collect();

        assert!(!metrics.iter().any(|m| m.name == "apollo.router.config"));
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.config.test")
        );
        assert!(!metrics.iter().any(|m| m.name == "apollo.router.entities"));
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.entities.test")
        );
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.connectors")
        );
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.schema.connectors")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_realtime_metrics() {
        let exporter = InMemoryMetricsExporter::default();
        let meter_provider = FilterMeterProvider::apollo_realtime(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                .build(),
        );
        let filtered = meter_provider.versioned_meter("filtered", "".into(), "".into(), None);
        filtered
            .u64_counter("apollo.router.operations.error")
            .init()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.mismatch")
            .init()
            .add(1, &[]);
        meter_provider.force_flush().unwrap();

        let metrics: Vec<_> = exporter
            .get_finished_metrics()
            .unwrap()
            .into_iter()
            .flat_map(|m| m.scope_metrics.into_iter())
            .flat_map(|m| m.metrics)
            .collect();
        // Matches
        assert!(
            metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.error")
        );

        // Mismatches
        assert!(
            !metrics
                .iter()
                .any(|m| m.name == "apollo.router.operations.mismatch")
        );
    }
}
