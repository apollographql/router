use std::collections::HashMap;
use std::sync::LazyLock;

use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider;
use parking_lot::Mutex;
use prometheus::Registry;
use tokio::task::block_in_place;
use tracing_subscriber::Layer;

use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider_internal;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::reload::otel::LayeredTracer;
use crate::plugins::telemetry::reload::otel::OPENTELEMETRY_TRACER_HANDLE;
use crate::plugins::telemetry::reload::otel::reload_fmt;

/// Activation is used to collect all the information that is needed when telemetry activate() is called.
/// It contains:
/// * meter providers
/// * trace provider
/// * trace propagation
/// * tracking of the most recent prometheus registry
/// * log format
///
/// This module correctly handles dropping of otel structures that may block in their own `Drop` implementation.
/// Meter and tracing providers must be dropped in a spawn blocking, therefore if activation is dropped
/// any such structs must be moved onto a blocking task.
/// Similarly, when apply is called we need to make sure that the providers that are being replaced
/// are also shut down in a safe way.
pub(crate) struct Activation {
    /// The new tracer provider. None means leave the existing one
    new_trace_provider: Option<opentelemetry_sdk::trace::TracerProvider>,

    /// The new tracer propagator. None means leave the existing one
    new_trace_propagator: Option<TextMapCompositePropagator>,

    /// The new metrics providers. Absent entry for a particular meter provider type
    /// means leave the existing one as is
    new_meter_providers: HashMap<MeterProviderType, FilterMeterProvider>,

    /// The registry that backs prometheus
    /// Unlike the other fields in this struct there is no noop implementation
    /// Therefore if this is None then Prometheus is not active
    /// This will be defaulted to the last applied registry via static unfortunately
    /// We can remove this static if eventually we have a facility for plugins to maintain state across reloads.
    prometheus_registry: Option<Registry>,

    /// The new format layer
    new_logging_fmt_layer: Option<Box<dyn Layer<LayeredTracer> + Send + Sync>>,

    /// Test instrumentation to track what components were set
    #[cfg(test)]
    test_instrumentation: TestInstrumentation,
}

#[cfg(test)]
#[derive(Default, Debug, Clone)]
pub(crate) struct TestInstrumentation {
    pub(crate) tracer_provider_set: bool,
    pub(crate) tracer_propagator_set: bool,
    pub(crate) meter_providers_added: std::collections::HashSet<MeterProviderType>,
    pub(crate) prometheus_registry_set: bool,
    pub(crate) logging_layer_set: bool,
}

/// Allows us to keep track of the last registry that was used. Not ideal. Plugins would be better to have state
/// that can be maintained across reloads.
static REGISTRY: LazyLock<Mutex<Option<Registry>>> = LazyLock::new(Default::default);

impl Activation {
    pub(crate) fn new() -> Activation {
        Activation {
            new_trace_provider: Default::default(),
            new_trace_propagator: Default::default(),
            new_meter_providers: Default::default(),
            // We can remove this is we allow state to be maintained across plugin reloads
            prometheus_registry: REGISTRY.lock().clone(),
            new_logging_fmt_layer: Default::default(),
            #[cfg(test)]
            test_instrumentation: Default::default(),
        }
    }

    pub(crate) fn with_logging(
        &mut self,
        logging_layer: Box<dyn Layer<LayeredTracer> + Send + Sync>,
    ) {
        self.new_logging_fmt_layer = Some(logging_layer);
        #[cfg(test)]
        {
            self.test_instrumentation.logging_layer_set = true;
        }
    }

    pub(crate) fn with_tracer_propagator(&mut self, tracer_propagator: TextMapCompositePropagator) {
        self.new_trace_propagator = Some(tracer_propagator);
        #[cfg(test)]
        {
            self.test_instrumentation.tracer_propagator_set = true;
        }
    }

    pub(crate) fn add_meter_providers(
        &mut self,
        meter_providers: impl IntoIterator<Item = (MeterProviderType, FilterMeterProvider)>,
    ) {
        for (meter_provider_type, meter_provider) in meter_providers {
            self.new_meter_providers
                .insert(meter_provider_type, meter_provider);
            #[cfg(test)]
            {
                self.test_instrumentation
                    .meter_providers_added
                    .insert(meter_provider_type);
            }
        }
    }

    pub(crate) fn with_tracer_provider(
        &mut self,
        tracer_provider: opentelemetry_sdk::trace::TracerProvider,
    ) {
        self.new_trace_provider = Some(tracer_provider);
        #[cfg(test)]
        {
            self.test_instrumentation.tracer_provider_set = true;
        }
    }

    pub(crate) fn with_prometheus_registry(&mut self, prometheus_registry: Option<Registry>) {
        self.prometheus_registry = prometheus_registry;
        #[cfg(test)]
        {
            self.test_instrumentation.prometheus_registry_set = true;
        }
    }

    pub(crate) fn prometheus_registry(&self) -> Option<Registry> {
        self.prometheus_registry.clone()
    }

    #[cfg(test)]
    pub(crate) fn test_instrumentation(&self) -> &TestInstrumentation {
        &self.test_instrumentation
    }
}

impl Activation {
    pub(crate) fn commit(mut self) {
        self.reload_tracing();
        self.reload_trace_propagation();
        self.reload_metrics();
        self.reload_logging();
        *REGISTRY.lock() = self.prometheus_registry.clone();
    }

    fn reload_tracing(&mut self) {
        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get()
            && let Some(tracer_provider) = self.new_trace_provider.take()
        {
            let tracer = tracer_provider
                .tracer_builder(GLOBAL_TRACER_NAME)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build();
            hot_tracer.reload(tracer);

            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);
            block_in_place(move || {
                drop(last_provider);
            });
        }
    }

    /// Reloads metrics providers, installing new ones and storing the old ones for safe shutdown on drop.
    pub(crate) fn reload_metrics(&mut self) {
        let global_meter_provider = meter_provider_internal();
        // Note that we are essentially swapping the new meter providers with the old.
        // The meter providers will be dealt with in Drop.
        for (meter_provider_type, meter_provider) in std::mem::take(&mut self.new_meter_providers) {
            self.new_meter_providers.insert(
                meter_provider_type,
                global_meter_provider.set(meter_provider_type, meter_provider),
            );
        }
    }

    fn reload_logging(&mut self) {
        if let Some(fmt_layer) = self.new_logging_fmt_layer.take() {
            reload_fmt(fmt_layer);
        }
    }

    fn reload_trace_propagation(&mut self) {
        if let Some(propagator) = self.new_trace_propagator.take() {
            opentelemetry::global::set_text_map_propagator(propagator);
        }
    }
}

/// When dropping activation we have to be careful to drop inside spawn tasks
/// Otel structures will perform blocking IO, so if drop happens in an async thread then it can cause issues.
/// The solution to this is to move the structs into a blocking task so that they can shut down safely.
impl Drop for Activation {
    fn drop(&mut self) {
        for (_, meter_provider) in std::mem::take(&mut self.new_meter_providers) {
            block_in_place(move || {
                drop(meter_provider);
            });
        }

        if let Some(tracer_provider) = self.new_trace_provider.take() {
            block_in_place(move || {
                drop(tracer_provider);
            });
        }
    }
}
