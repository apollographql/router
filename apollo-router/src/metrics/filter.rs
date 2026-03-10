use std::sync::Arc;
use std::time::Duration;

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
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use regex::Regex;

use super::NoopInstrumentProvider;

/// Wrapper for different meter provider types
#[derive(Clone)]
enum MeterProviderInner {
    Sdk(SdkMeterProvider),
    /// Noop provider - returns noop instruments without going through OTel SDK.
    /// Used as a placeholder to avoid OTel SDK errors during startup/reconfiguration.
    Noop,
}

impl OtelMeterProvider for MeterProviderInner {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        match self {
            MeterProviderInner::Sdk(p) => p.meter_with_scope(scope),
            MeterProviderInner::Noop => Meter::new(Arc::new(NoopInstrumentProvider)),
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

    #[cfg(test)]
    pub(crate) fn all(delegate: SdkMeterProvider) -> Self {
        FilterMeterProvider::builder()
            .delegate(MeterProviderInner::Sdk(delegate))
            .build()
    }

    /// Create a noop filter provider that returns noop instruments.
    /// Used as a placeholder to avoid OTel SDK errors during startup/reconfiguration.
    pub(crate) fn noop() -> Self {
        FilterMeterProvider {
            delegate: MeterProviderInner::Noop,
            deny: None,
            allow: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn force_flush(&self) -> OTelSdkResult {
        match &self.delegate {
            MeterProviderInner::Sdk(p) => p.force_flush(),
            MeterProviderInner::Noop => Ok(()),
        }
    }

    pub(crate) fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        match &self.delegate {
            MeterProviderInner::Sdk(p) => p.shutdown_with_timeout(timeout),
            MeterProviderInner::Noop => Ok(()),
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
        fn $name(&self, builder: AsyncInstrumentBuilder<'_, $wrapper<$ty>, $ty>) -> $wrapper<$ty> {
            let is_filtered = match (&self.deny, &self.allow) {
                // Deny match takes precedence over allow match
                (Some(deny), _) if deny.is_match(&builder.name) => true,
                (_, Some(allow)) if !allow.is_match(&builder.name) => true,
                (_, _) => false,
            };

            // For filtered observable instruments, route through the noop meter.
            // This avoids registering callbacks with the SDK which would log
            // errors about views not producing measures in OTel 0.31+.
            if is_filtered {
                return self.noop.$name(builder.name.clone()).build();
            }

            // Extract builder fields before consuming callbacks
            let name = builder.name;
            let description = builder.description;
            let unit = builder.unit;

            // Wrap callbacks in Arc for sharing
            let shared_callbacks: Vec<
                std::sync::Arc<
                    dyn Fn(&dyn opentelemetry::metrics::AsyncInstrument<$ty>) + Send + Sync,
                >,
            > = builder
                .callbacks
                .into_iter()
                .map(std::sync::Arc::from)
                .collect();

            let mut b = self.delegate.$name(name);
            if let Some(desc) = &description {
                b = b.with_description(desc.clone());
            }
            if let Some(u) = &unit {
                b = b.with_unit(u.clone());
            }
            // Forward callbacks to the delegate
            for callback in shared_callbacks {
                let cb = std::sync::Arc::clone(&callback);
                b = b.with_callback(move |observer| cb(observer));
            }
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
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
    use opentelemetry_sdk::runtime;

    use crate::metrics::filter::FilterMeterProvider;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_metrics() {
        let exporter = InMemoryMetricExporter::default();
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

        let finished = exporter.get_finished_metrics().unwrap();
        let metric_names: Vec<_> = finished
            .iter()
            .flat_map(|m| m.scope_metrics())
            .flat_map(|m| m.metrics())
            .map(|m| m.name())
            .collect();

        // Matches allow
        assert!(metric_names.contains(&"apollo.router.operations.test"));

        assert!(metric_names.contains(&"apollo.router.operations"));

        assert!(metric_names.contains(&"apollo.graphos.cloud.test"));

        assert!(metric_names.contains(&"apollo.router.lifecycle.api_schema"));

        assert!(metric_names.contains(&"apollo.router.operations.connectors"));
        assert!(metric_names.contains(&"apollo.router.schema.connectors"));

        // Mismatches allow
        assert!(!metric_names.contains(&"apollo.router.unknown.test"));

        // Matches deny
        assert!(!metric_names.contains(&"apollo.router.operations.error"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_description_and_unit() {
        let exporter = InMemoryMetricExporter::default();
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

        let finished = exporter.get_finished_metrics().unwrap();
        let found = finished
            .iter()
            .flat_map(|m| m.scope_metrics())
            .flat_map(|m| m.metrics())
            .any(|m| {
                m.name() == "apollo.router.operations"
                    && m.description() == "desc"
                    && m.unit() == "ms"
            });
        assert!(found);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_public_metrics() {
        let exporter = InMemoryMetricExporter::default();
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

        let finished = exporter.get_finished_metrics().unwrap();
        let metric_names: Vec<_> = finished
            .iter()
            .flat_map(|m| m.scope_metrics())
            .flat_map(|m| m.metrics())
            .map(|m| m.name())
            .collect();

        assert!(!metric_names.contains(&"apollo.router.config"));
        assert!(!metric_names.contains(&"apollo.router.config.test"));
        assert!(!metric_names.contains(&"apollo.router.entities"));
        assert!(!metric_names.contains(&"apollo.router.entities.test"));
        assert!(!metric_names.contains(&"apollo.router.operations.connectors"));
        assert!(!metric_names.contains(&"apollo.router.schema.connectors"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_private_realtime_metrics() {
        let exporter = InMemoryMetricExporter::default();
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

        let finished = exporter.get_finished_metrics().unwrap();
        let metric_names: Vec<_> = finished
            .iter()
            .flat_map(|m| m.scope_metrics())
            .flat_map(|m| m.metrics())
            .map(|m| m.name())
            .collect();
        // Matches
        assert!(metric_names.contains(&"apollo.router.operations.error"));

        // Mismatches
        assert!(!metric_names.contains(&"apollo.router.operations.mismatch"));
    }
}
