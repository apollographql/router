//! Telemetry activation and reloading logic.
//!
//! This module contains all the complex logic for activating telemetry components
//! and handling reloads while maintaining metric continuity.

use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::Builder;
use tower::BoxError;

use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider_internal;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::TracingCommon;
use crate::plugins::telemetry::config_new::spans::Spans;
use crate::plugins::telemetry::metrics::MetricsBuilder;
use crate::plugins::telemetry::metrics::MetricsConfigurator;
use crate::plugins::telemetry::metrics::prometheus::commit_prometheus;
use crate::plugins::telemetry::reload::OPENTELEMETRY_TRACER_HANDLE;
use crate::plugins::telemetry::tracing::TracingConfigurator;

pub(crate) const GLOBAL_TRACER_NAME: &str = "apollo-router";

/// Manages the activation state of telemetry components.
///
/// This struct holds all the meter providers and tracer providers that need to be
/// managed during telemetry reloads. It ensures proper cleanup of old providers
/// and atomic activation of new ones.
#[derive(Default)]
pub(crate) struct TelemetryActivation {
    pub(crate) tracer_provider: Option<opentelemetry_sdk::trace::TracerProvider>,
    // We have to have separate meter providers for prometheus metrics so that they don't get zapped on router reload.
    pub(crate) public_meter_provider: Option<FilterMeterProvider>,
    pub(crate) public_prometheus_meter_provider: Option<FilterMeterProvider>,
    pub(crate) private_meter_provider: Option<FilterMeterProvider>,
    pub(crate) private_realtime_meter_provider: Option<FilterMeterProvider>,
    // Store whether we should refresh the tracer provider during activation
    pub(crate) should_refresh_tracer: bool,
    pub(crate) is_active: bool,
}

impl TelemetryActivation {
    /// Creates a new activation state from the built metrics and tracing components.
    pub(crate) fn from_builders(
        public_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
        apollo_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
        apollo_realtime_meter_provider_builder: opentelemetry_sdk::metrics::MeterProviderBuilder,
        prometheus_meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
        tracer_provider: opentelemetry_sdk::trace::TracerProvider,
        should_refresh_tracer: bool,
    ) -> Self {
        Self {
            tracer_provider: Some(tracer_provider),
            public_meter_provider: Some(FilterMeterProvider::public(
                public_meter_provider_builder.build(),
            )),
            public_prometheus_meter_provider: prometheus_meter_provider
                .map(FilterMeterProvider::public),
            private_meter_provider: Some(FilterMeterProvider::private(
                apollo_meter_provider_builder.build(),
            )),
            private_realtime_meter_provider: Some(FilterMeterProvider::private_realtime(
                apollo_realtime_meter_provider_builder.build(),
            )),
            should_refresh_tracer,
            is_active: false,
        }
    }

    /// Reloads all meter providers, replacing the global meter providers with new ones
    /// and properly shutting down the old ones.
    pub(crate) fn reload_metrics(&mut self) {
        let meter_provider = meter_provider_internal();
        commit_prometheus();
        let mut old_meter_providers: [Option<FilterMeterProvider>; 4] = Default::default();

        old_meter_providers[0] = meter_provider.set(
            MeterProviderType::PublicPrometheus,
            self.public_prometheus_meter_provider.take(),
        );

        old_meter_providers[1] = meter_provider.set(
            MeterProviderType::Apollo,
            self.private_meter_provider.take(),
        );

        old_meter_providers[2] = meter_provider.set(
            MeterProviderType::ApolloRealtime,
            self.private_realtime_meter_provider.take(),
        );

        old_meter_providers[3] =
            meter_provider.set(MeterProviderType::Public, self.public_meter_provider.take());

        Self::checked_meter_shutdown(old_meter_providers);
    }

    /// Activates the tracer provider by setting it as the global tracer provider
    /// and updating the hot tracer reload handle. Only refreshes if needed.
    pub(crate) fn activate_tracer(&mut self) -> Result<(), BoxError> {
        if !self.should_refresh_tracer {
            ::tracing::debug!("Skipping tracer refresh - tracing configuration unchanged");
            return Ok(());
        }

        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            let tracer_provider = self
                .tracer_provider
                .take()
                .expect("must have new tracer_provider");

            let tracer = tracer_provider
                .tracer_builder(GLOBAL_TRACER_NAME)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build();
            hot_tracer.reload(tracer);

            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);

            // Shut down the old provider in the background
            Telemetry::checked_global_tracer_shutdown(last_provider);

            ::tracing::debug!("Tracer provider refreshed due to configuration changes");
        }
        Ok(())
    }

    /// Safely shuts down meter providers in the background to avoid blocking activation.
    pub(crate) fn checked_meter_shutdown(meters: [Option<FilterMeterProvider>; 4]) {
        for meter_provider in meters.into_iter().flatten() {
            Telemetry::checked_spawn_task(Box::new(move || {
                if let Err(e) = meter_provider.shutdown() {
                    ::tracing::error!(error = %e, "failed to shutdown meter provider")
                }
            }));
        }
    }
}

/// Sets up a tracing exporter by applying its configuration to the tracer builder.
///
/// This is a generic function that works with any type implementing TracingConfigurator.
/// It only applies the configuration if the exporter is enabled.
pub(crate) fn setup_tracing<T: TracingConfigurator>(
    mut builder: Builder,
    configurator: &T,
    tracing_config: &TracingCommon,
    spans_config: &Spans,
) -> Result<Builder, BoxError> {
    if configurator.enabled() {
        builder = configurator.apply(builder, tracing_config, spans_config)?;
    }
    Ok(builder)
}

/// Sets up a metrics exporter by applying its configuration to the metrics builder.
///
/// This is a generic function that works with any type implementing MetricsConfigurator.
/// It only applies the configuration if the exporter is enabled.
pub(crate) fn setup_metrics_exporter<T: MetricsConfigurator>(
    mut builder: MetricsBuilder,
    configurator: &T,
    metrics_common: &MetricsCommon,
) -> Result<MetricsBuilder, BoxError> {
    if configurator.enabled() {
        builder = configurator.apply(builder, metrics_common)?;
    }
    Ok(builder)
}
