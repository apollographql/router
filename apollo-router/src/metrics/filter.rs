use std::sync::Arc;

use buildstructor::buildstructor;
use opentelemetry::InstrumentationScope;
use opentelemetry::metrics::AsyncInstrumentBuilder;
use opentelemetry::metrics::Counter;
use opentelemetry::metrics::Gauge;
use opentelemetry::metrics::Histogram;
use opentelemetry::metrics::HistogramBuilder;
use opentelemetry::metrics::InstrumentBuilder;
use opentelemetry::metrics::InstrumentProvider;
use opentelemetry::metrics::Meter;
use opentelemetry::metrics::MeterProvider as OtelMeterProvider;
use opentelemetry::metrics::ObservableCounter;
use opentelemetry::metrics::ObservableGauge;
use opentelemetry::metrics::ObservableUpDownCounter;
use opentelemetry::metrics::UpDownCounter;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use regex::Regex;

/// Noop InstrumentProvider - all methods use the default trait implementations
/// which return noop instruments.
struct NoopInstrumentProvider;
impl InstrumentProvider for NoopInstrumentProvider {}

/// Wrapper for different meter provider types
#[derive(Clone)]
enum MeterProviderInner {
    Sdk(SdkMeterProvider),
    Dynamic(Arc<dyn OtelMeterProvider + Send + Sync>),
}

impl OtelMeterProvider for MeterProviderInner {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        match self {
            MeterProviderInner::Sdk(p) => p.meter_with_scope(scope),
            MeterProviderInner::Dynamic(p) => p.meter_with_scope(scope),
        }
    }
}

#[derive(Clone)]
pub(crate) struct FilterMeterProvider {
    delegate: MeterProviderInner,
    deny: Option<Regex>,
    allow: Option<Regex>,
}

#[buildstructor]
impl FilterMeterProvider {
    #[builder]
    fn new(delegate: MeterProviderInner, deny: Option<Regex>, allow: Option<Regex>) -> Self {
        FilterMeterProvider {
            delegate,
            deny,
            allow,
        }
    }

    fn get_private_realtime_regex() -> Regex {
        Regex::new(r"apollo\.router\.operations\.(?:error|fetch\.duration)")
            .expect("regex should have been valid")
    }

    pub(crate) fn apollo(delegate: SdkMeterProvider) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Sdk(delegate))
            .allow(
                Regex::new(
                  r"apollo\.(graphos\.cloud|router\.(operations?|lifecycle|config|schema|query|query_planning|telemetry|instance|graphql_error))(\..*|$)|apollo_router_uplink_fetch_count_total|apollo_router_uplink_fetch_duration_seconds",
                )
                .expect("regex should have been valid"),
            )
            .deny(Self::get_private_realtime_regex().clone())
            .build()
    }

    pub(crate) fn apollo_realtime(delegate: SdkMeterProvider) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Sdk(delegate))
            .allow(Self::get_private_realtime_regex().clone())
            .build()
    }

    pub(crate) fn public(delegate: SdkMeterProvider) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Sdk(delegate))
            .deny(
                Regex::new(r"apollo\.router\.(config|entities|instance|operations\.(connectors|fetch|request_size|response_size|error)|schema\.connectors)(\..*|$)")
                    .expect("regex should have been valid"),
            )
            .build()
    }

    /// Create a public filter from a dynamic meter provider (e.g., from opentelemetry::global::meter_provider())
    pub(crate) fn public_dynamic(delegate: Arc<dyn OtelMeterProvider + Send + Sync>) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Dynamic(delegate))
            .deny(
                Regex::new(r"apollo\.router\.(config|entities|instance|operations\.(connectors|fetch|request_size|response_size|error)|schema\.connectors)(\..*|$)")
                    .expect("regex should have been valid"),
            )
            .build()
    }

    #[cfg(test)]
    pub(crate) fn all(delegate: SdkMeterProvider) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Sdk(delegate))
            .build()
    }

    #[cfg(test)]
    pub(crate) fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        match &self.delegate {
            MeterProviderInner::Sdk(p) => p.force_flush(),
            MeterProviderInner::Dynamic(_) => Ok(()),
        }
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
        fn $name(&self, builder: InstrumentBuilder<'_, $wrapper<$ty>>) -> $wrapper<$ty> {
            let meter = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&builder.name) => &self.noop,
                (_, Some(allow)) if !allow.is_match(&builder.name) => &self.noop,
                (_, _) => &self.delegate,
            };
            let mut b = meter.$name(builder.name.clone());
            if let Some(description) = &builder.description {
                b = b.with_description(description.clone());
            }
            if let Some(unit) = &builder.unit {
                b = b.with_unit(unit.clone());
            }
            b.build()
        }
    };
}

macro_rules! filter_histogram_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(&self, builder: HistogramBuilder<'_, $wrapper<$ty>>) -> $wrapper<$ty> {
            let meter = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&builder.name) => &self.noop,
                (_, Some(allow)) if !allow.is_match(&builder.name) => &self.noop,
                (_, _) => &self.delegate,
            };
            let mut b = meter.$name(builder.name.clone());
            if let Some(description) = &builder.description {
                b = b.with_description(description.clone());
            }
            if let Some(unit) = &builder.unit {
                b = b.with_unit(unit.clone());
            }
            if let Some(boundaries) = &builder.boundaries {
                b = b.with_boundaries(boundaries.clone());
            }
            b.build()
        }
    };
}

macro_rules! filter_observable_instrument_fn {
    ($name:ident, $ty:ty, $wrapper:ident) => {
        fn $name(
            &self,
            builder: AsyncInstrumentBuilder<'_, $wrapper<$ty>, $ty>,
        ) -> $wrapper<$ty> {
            let meter = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&builder.name) => &self.noop,
                (_, Some(allow)) if !allow.is_match(&builder.name) => &self.noop,
                (_, _) => &self.delegate,
            };
            let mut b = meter.$name(builder.name.clone());
            if let Some(description) = &builder.description {
                b = b.with_description(description.clone());
            }
            if let Some(unit) = &builder.unit {
                b = b.with_unit(unit.clone());
            }
            // Note: Callbacks from the original builder are not forwarded
            // as they may contain references to the original instrument.
            // Callbacks should be set on the result if needed.
            b.build()
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

impl OtelMeterProvider for FilterMeterProvider {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        Meter::new(Arc::new(FilteredInstrumentProvider {
            noop: Meter::new(Arc::new(NoopInstrumentProvider)),
            delegate: self.delegate.meter_with_scope(scope),
            deny: self.deny.clone(),
            allow: self.allow.clone(),
        }))
    }
}

#[cfg(test)]
mod test {
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
        let filtered = meter_provider.meter("filtered");
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
        let filtered = meter_provider.meter("filtered");
        filtered
            .u64_counter("apollo.router.operations")
            .with_description("desc")
            .with_unit("ms")
            .build()
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
    async fn test_public_metrics() {
        let exporter = InMemoryMetricsExporter::default();
        let meter_provider = FilterMeterProvider::public(
            MeterProviderBuilder::default()
                .with_reader(PeriodicReader::builder(exporter.clone(), runtime::Tokio).build())
                .build(),
        );
        let filtered = meter_provider.meter("filtered");
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
        let filtered = meter_provider.meter("filtered");
        filtered
            .u64_counter("apollo.router.operations.error")
            .build()
            .add(1, &[]);
        filtered
            .u64_counter("apollo.router.operations.mismatch")
            .build()
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
