//! Telemetry plugin lifecycle management.

use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider_internal;
use crate::metrics::aggregation::MeterProviderType;
use crate::plugins::telemetry::metrics::prometheus::commit_prometheus;
use crate::plugins::telemetry::Telemetry;

/// Manages the lifecycle of telemetry providers (tracing and metrics).
/// This struct tracks active providers and handles their shutdown.
pub(crate) struct TelemetryActivation {
    pub(crate) tracer_provider: Option<opentelemetry_sdk::trace::TracerProvider>,
    // We have to have separate meter providers for prometheus metrics so that they don't get zapped on router reload.
    pub(crate) public_meter_provider: Option<FilterMeterProvider>,
    pub(crate) public_prometheus_meter_provider: Option<FilterMeterProvider>,
    pub(crate) private_meter_provider: Option<FilterMeterProvider>,
    pub(crate) private_realtime_meter_provider: Option<FilterMeterProvider>,
    pub(crate) is_active: bool,
}

impl TelemetryActivation {
    /// Reloads metrics providers, shutting down old ones and installing new ones.
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

    /// Safely shuts down meter providers in background tasks.
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