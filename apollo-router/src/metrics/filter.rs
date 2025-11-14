use std::borrow::Cow;
use std::sync::Arc;

use buildstructor::buildstructor;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider as OtelMeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::UpDownCounter;
use regex::Regex;

//`opentelemetry::global::GlobalMeterProvider` type alias was made private so we recreate it here
type GlobalMeterProvider = Arc<dyn opentelemetry::metrics::MeterProvider + Send + Sync>;

#[derive(Clone)]
pub(crate) enum MeterProvider {
    Regular(opentelemetry_sdk::metrics::SdkMeterProvider),
    Global(GlobalMeterProvider),
}

impl MeterProvider {
    fn versioned_meter(
        &self,
        name: &'static str,
        version: Option<impl Into<Cow<'static, str>>>,
        schema_url: Option<impl Into<Cow<'static, str>>>,
        attributes: Option<Vec<KeyValue>>,
    ) -> Meter {
        match &self {
            MeterProvider::Regular(provider) => {
                let mut builder = InstrumentationScope::builder(name);
                if let Some(v) = version {
                    builder = builder.with_version(v.into());
                }
                if let Some(s) = schema_url {
                    builder = builder.with_schema_url(s.into());
                }
                if let Some(ref attrs) = attributes {
                    builder = builder.with_attributes(attrs.clone());
                }
                provider.meter_with_scope(builder.build())
            }
            MeterProvider::Global(provider) => {
                let mut builder = InstrumentationScope::builder(name);
                if let Some(v) = version {
                    builder = builder.with_version(v.into());
                }
                if let Some(s) = schema_url {
                    builder = builder.with_schema_url(s.into());
                }
                if let Some(ref attrs) = attributes {
                    builder = builder.with_attributes(attrs.clone());
                }
                provider.meter_with_scope(builder.build())
            }
        }
    }

    fn meter_with_scope(&self, scope: &InstrumentationScope) -> Meter {
        match &self {
            MeterProvider::Regular(provider) => provider.meter_with_scope(scope.clone()),
            MeterProvider::Global(provider) => provider.meter_with_scope(scope.clone()),
        }
    }

    #[cfg(test)]
    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
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

impl From<GlobalMeterProvider> for MeterProvider {
    fn from(provider: GlobalMeterProvider) -> Self {
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

    #[cfg(test)]
    pub(crate) fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
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
            builder: opentelemetry::metrics::InstrumentBuilder<'_, $wrapper<$ty>>,
        ) -> $wrapper<$ty> {
            let name = builder.name.to_string();
            match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, Some(allow)) if !allow.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, _) => {
                    let mut instrument_builder = self.delegate.$name(builder.name);
                    if let Some(ref description) = builder.description {
                        instrument_builder =
                            instrument_builder.with_description(description.clone());
                    }
                    if let Some(ref unit) = builder.unit {
                        instrument_builder = instrument_builder.with_unit(unit.clone());
                    }
                    instrument_builder.build()
                }
            }
        }
    };
}

macro_rules! filter_histogram_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            builder: opentelemetry::metrics::HistogramBuilder<'_, $wrapper<$ty>>,
        ) -> $wrapper<$ty> {
            let name = builder.name.to_string();
            match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, Some(allow)) if !allow.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, _) => {
                    let mut instrument_builder = self.delegate.$name(builder.name);
                    if let Some(ref description) = builder.description {
                        instrument_builder =
                            instrument_builder.with_description(description.clone());
                    }
                    if let Some(ref unit) = builder.unit {
                        instrument_builder = instrument_builder.with_unit(unit.clone());
                    }
                    instrument_builder.build()
                }
            }
        }
    };
}

macro_rules! filter_observable_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            builder: opentelemetry::metrics::AsyncInstrumentBuilder<'_, $wrapper<$ty>, $ty>,
        ) -> $wrapper<$ty> {
            let name = builder.name.to_string();
            match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, Some(allow)) if !allow.is_match(&name) => self.noop.$name(builder.name).build(),
                (_, _) => {
                    let mut instrument_builder = self.delegate.$name(builder.name);
                    for callback in builder.callbacks {
                        instrument_builder = instrument_builder.with_callback(callback);
                    }
                    if let Some(ref description) = builder.description {
                        instrument_builder =
                            instrument_builder.with_description(description.clone());
                    }
                    if let Some(ref unit) = builder.unit {
                        instrument_builder = instrument_builder.with_unit(unit.clone());
                    }
                    instrument_builder.build()
                }
            }
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

    filter_histogram_fn!(u64_histogram, u64, Histogram);
    filter_histogram_fn!(f64_histogram, f64, Histogram);

    filter_instrument_fn!(i64_up_down_counter, i64, UpDownCounter);
    filter_instrument_fn!(f64_up_down_counter, f64, UpDownCounter);

    filter_observable_instrument_fn!(i64_observable_up_down_counter, i64, ObservableUpDownCounter);
    filter_observable_instrument_fn!(f64_observable_up_down_counter, f64, ObservableUpDownCounter);

    filter_observable_instrument_fn!(f64_observable_gauge, f64, ObservableGauge);
    filter_observable_instrument_fn!(i64_observable_gauge, i64, ObservableGauge);
    filter_observable_instrument_fn!(u64_observable_gauge, u64, ObservableGauge);
}

impl opentelemetry::metrics::MeterProvider for FilterMeterProvider {
    fn meter(&self, name: &'static str) -> Meter {
        Meter::new(Arc::new(FilteredInstrumentProvider {
            noop: opentelemetry::global::meter_provider().meter(""),
            delegate: self
                .delegate
                .versioned_meter(name, None::<&str>, None::<&str>, None),
            deny: self.deny.clone(),
            allow: self.allow.clone(),
        }))
    }
    fn meter_with_scope(&self, scope: opentelemetry::InstrumentationScope) -> Meter {
        Meter::new(Arc::new(FilteredInstrumentProvider {
            noop: opentelemetry::global::meter_provider().meter(""),
            delegate: self.delegate.meter_with_scope(&scope),
            deny: self.deny.clone(),
            allow: self.allow.clone(),
        }))
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::{global, InstrumentationScope};
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::PeriodicReader;

    use crate::metrics::filter::FilterMeterProvider;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_metrics() {
        let exporter = InMemoryMetricExporter::default();
        let meter_provider = FilterMeterProvider::apollo(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build(),
        );
        let filtered = meter_provider
                .meter_with_scope(InstrumentationScope::builder("filtered").build());
        // Matches allow
        filtered
            .u64_counter("apollo.router.operations")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.test")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.graphos.cloud.test")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.query_planning.test")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.lifecycle.api_schema")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.connectors")
            .build()
            .add(1, &[]);
        filtered
            .u64_observable_gauge("apollo.router.schema.connectors")
            .with_callback(move |observer| observer.observe(1, &[]))
            .build();

        // Mismatches allow
        filtered
            .u64_counter("apollo.router.unknown.test")
            .build()
            .add(1, &[]);

        // Matches deny
        filtered
            .u64_counter("apollo.router.operations.error")
            .build()
            .add(1, &[]);

        meter_provider.force_flush().unwrap();

        let resource_metrics = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .collect();

        // Matches allow
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.test")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.graphos.cloud.test")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.lifecycle.api_schema")
        );

        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.connectors")
        );
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.schema.connectors")
        );

        // Mismatches allow
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.unknown.test")
        );

        // Matches deny
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.error")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_description_and_unit() {
        let exporter = InMemoryMetricExporter::default();
        let meter_provider = FilterMeterProvider::apollo(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build(),
        );
        let filtered = meter_provider
            .meter_with_scope(InstrumentationScope::builder("filtered").build());
        filtered
            .u64_counter("apollo.router.operations")
            .with_description("desc")
            .with_unit("ms")
            .build()
            .add(1, &[]);
        meter_provider.force_flush().unwrap();

        let resource_metrics = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .collect();
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations"
                    && m.description() == "desc"
                    && m.unit() == "ms")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_public_metrics_using_meter_provider() {
        let exporter = InMemoryMetricExporter::default();
        test_public_metrics(
            exporter.clone(),
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build(),
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_public_metrics_using_global_meter_provider() {
        let exporter = InMemoryMetricExporter::default();
        global::set_meter_provider(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build()
        );
        test_public_metrics(
            exporter.clone(),
            global::meter_provider(),
        )
        .await;
    }
    async fn test_public_metrics<T: Into<super::MeterProvider>>(
        exporter: InMemoryMetricExporter,
        meter_provider: T,
    ) {
        let meter_provider = FilterMeterProvider::public(meter_provider);
        let filtered = meter_provider
            .meter_with_scope(InstrumentationScope::builder("filtered").build());
        filtered
            .u64_counter("apollo.router.config")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.config.test")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.entities")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.entities.test")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.connectors")
            .build()
            .add(1, &[]);
        filtered
            .u64_observable_gauge("apollo.router.schema.connectors")
            .with_callback(move |observer| observer.observe(1, &[]))
            .build();
        meter_provider.force_flush().unwrap();

        let resource_metrics = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .collect();

        assert!(!metrics.iter().any(|m| m.name() == "apollo.router.config"));
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.config.test")
        );
        assert!(!metrics.iter().any(|m| m.name() == "apollo.router.entities"));
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.entities.test")
        );
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.connectors")
        );
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.schema.connectors")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_realtime_metrics() {
        let exporter = InMemoryMetricExporter::default();
        let meter_provider = FilterMeterProvider::apollo_realtime(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build(),
        );
        let filtered = meter_provider
            .meter_with_scope(InstrumentationScope::builder("filtered").build());
        filtered
            .u64_counter("apollo.router.operations.error")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.mismatch")
            .build()
            .add(1, &[]);
        meter_provider.force_flush().unwrap();

        let resource_metrics = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = resource_metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .collect();
        // Matches
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.error")
        );

        // Mismatches
        assert!(
            !metrics
                .iter()
                .any(|m| m.name() == "apollo.router.operations.mismatch")
        );
    }
}
