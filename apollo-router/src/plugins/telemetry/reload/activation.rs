//! Telemetry plugin lifecycle management.

use std::collections::HashMap;
use std::sync::LazyLock;

use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider;
use parking_lot::Mutex;
use prometheus::Registry;
use tokio::runtime::Handle;

use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider_internal;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::reload::otel::OPENTELEMETRY_TRACER_HANDLE;

/// Manages the lifecycle of telemetry providers (tracing and metrics).
/// This struct tracks active providers and handles their shutdown.
pub(crate) struct Activation {
    /// The new tracer provider. None means leave the existing one
    tracer_provider: Option<opentelemetry_sdk::trace::TracerProvider>,
    /// The new tracer propagator. None means leave the existing one
    tracer_propagator: Option<TextMapCompositePropagator>,
    /// The new metrics providers. Absent means leave the existing one
    meter_providers: HashMap<MeterProviderType, FilterMeterProvider>,

    /// The registry that backs prometheus
    /// This will be defaulted to the last applied registry via static unfortunately
    /// We can further remove this if eventually we have a facility for plugins to maintain state across reloads.
    prometheus_registry: Option<Registry>,
}

/// Allows us to keep track of the last registry that was used. Not ideal. Plugins would be better to have state
/// that can be maintained across reloads.
static REGISTRY: LazyLock<Mutex<Option<Registry>>> = LazyLock::new(Default::default);

impl Activation {
    pub(crate) fn new() -> Activation {
        Activation {
            tracer_provider: Default::default(),
            tracer_propagator: Default::default(),
            meter_providers: Default::default(),
            // We can remove this is we allow state to be maintained across plugin reloads
            prometheus_registry: REGISTRY.lock().clone(),
        }
    }

    pub(crate) fn with_tracer_propagator(&mut self, tracer_propagator: TextMapCompositePropagator) {
        self.tracer_propagator = Some(tracer_propagator);
    }
    pub(crate) fn add_meter_providers(
        &mut self,
        meter_providers: impl Iterator<Item = (MeterProviderType, FilterMeterProvider)>,
    ) {
        self.meter_providers.extend(meter_providers);
    }

    pub(crate) fn with_tracer_provider(
        &mut self,
        tracer_provider: opentelemetry_sdk::trace::TracerProvider,
    ) {
        self.tracer_provider = Some(tracer_provider);
    }

    pub(crate) fn with_prometheus_registry(&mut self, prometheus_registry: Option<Registry>) {
        self.prometheus_registry = prometheus_registry;
    }

    pub(crate) fn prometheus_registry(&self) -> Option<Registry> {
        self.prometheus_registry.clone()
    }
}

impl Activation {
    pub(crate) fn commit(mut self) {
        self.reload_tracing();
        self.reload_metrics();
        *REGISTRY.lock() = self.prometheus_registry.clone();
    }

    fn reload_tracing(&mut self) {
        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            if let Some(tracer_provider) = self.tracer_provider.take() {
                let tracer = tracer_provider
                    .tracer_builder(GLOBAL_TRACER_NAME)
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .build();
                hot_tracer.reload(tracer);

                let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);
                checked_spawn_task(Box::new(move || {
                    drop(last_provider);
                }));
            }
            if let Some(propagator) = self.tracer_propagator.take() {
                opentelemetry::global::set_text_map_propagator(propagator);
            }
        }
    }

    /// Reloads metrics providers, shutting down old ones and installing new ones.
    /// With the new semantics:
    /// - If a field contains Some(meter_provider), we install that meter provider
    /// - If a field contains None, we ignore it
    /// - None is never passed to set() in this method since we always want to update all types
    pub(crate) fn reload_metrics(&mut self) {
        let global_meter_provider = meter_provider_internal();
        // Note that we are essentially swapping the new meter providers with the old.
        // The meter providers will be dealt with in Drop.
        for (meter_provider_type, meter_provider) in std::mem::take(&mut self.meter_providers) {
            self.meter_providers.insert(
                meter_provider_type,
                global_meter_provider.set(meter_provider_type, meter_provider),
            );
        }
    }
}

/// When dropping activation we have to be careful to drop inside spawn tasks
impl Drop for Activation {
    fn drop(&mut self) {
        for (meter_provider_type, meter_provider) in std::mem::take(&mut self.meter_providers) {
            checked_spawn_task(Box::new(move || {
                if let Err(e) = meter_provider.shutdown() {
                    ::tracing::error!(error = %e, "meter.provider.type" = %meter_provider_type, "failed to shutdown meter provider")
                }
            }));
        }

        if let Some(tracer_provider) = self.tracer_provider.take() {
            checked_spawn_task(Box::new(move || {
                drop(tracer_provider);
            }));
        }
    }
}

/// If we are in an tokio async context, use `spawn_blocking()`, if not just execute the
/// task.
/// Note:
///  - If we use spawn_blocking, then tokio looks after waiting for the task to
///    terminate
///  - We could spawn a thread to execute the task, but if the process terminated that would
///    cause the thread to terminate which isn't ideal. Let's just run it in the current
///    thread. This won't affect router performance since that will always be within the
///    context of tokio.
fn checked_spawn_task(task: Box<dyn FnOnce() + Send + 'static>) {
    match Handle::try_current() {
        Ok(hdl) => {
            hdl.spawn_blocking(move || {
                task();
            });
            // We don't join here since we can't await or block_on()
        }
        Err(_err) => {
            task();
        }
    }
}
