//! Telemetry activation state container
//!
//! This module provides the [`Activation`] type which acts as a container for all telemetry
//! components that will be activated when the router is ready to commit configuration changes.
//!
//! ## Purpose
//!
//! The [`Activation`] struct collects new telemetry components during the preparation phase:
//! - Meter providers (for metrics)
//! - Tracer provider (for distributed tracing)
//! - Trace propagation configuration
//! - Prometheus registry (if enabled)
//! - Logging format layer
//!
//! ## Safe Resource Management
//!
//! OpenTelemetry providers perform blocking I/O during shutdown, which can deadlock if executed
//! on async runtime threads. This module ensures safety by:
//!
//! 1. **During commit**: Old providers being replaced are moved to blocking tasks for safe shutdown
//! 2. **During drop**: Any uncommitted providers are moved to blocking tasks for cleanup
//!
//! This prevents blocking the async runtime while ensuring all resources are properly cleaned up.

use std::collections::HashMap;
use std::sync::LazyLock;

use opentelemetry::InstrumentationScope;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider;
use parking_lot::Mutex;
use prometheus::Registry;
use tokio::task::spawn_blocking;
use tracing_subscriber::Layer;

use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::metrics::meter_provider_internal;
use crate::plugins::telemetry::GLOBAL_TRACER_NAME;
use crate::plugins::telemetry::reload::otel::LayeredTracer;
use crate::plugins::telemetry::reload::otel::OPENTELEMETRY_TRACER_HANDLE;
use crate::plugins::telemetry::reload::otel::reload_fmt;

/// State container for telemetry components to be activated.
///
/// Collects new telemetry providers and configuration during the preparation phase,
/// then atomically applies them during the activation phase via [`Activation::commit()`].
pub(crate) struct Activation {
    /// The new tracer provider. None means leave the existing one
    new_trace_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,

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
    pub(crate) fn new() -> Self {
        Self {
            new_trace_provider: None,
            new_trace_propagator: None,
            new_meter_providers: HashMap::default(),
            // We can remove this is we allow state to be maintained across plugin reloads
            prometheus_registry: REGISTRY.lock().clone(),
            new_logging_fmt_layer: None,
            #[cfg(test)]
            test_instrumentation: TestInstrumentation::default(),
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
        tracer_provider: opentelemetry_sdk::trace::SdkTracerProvider,
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
    /// Commits the prepared telemetry state to global OpenTelemetry providers (Phase 2 of reload lifecycle).
    ///
    /// This method atomically updates all global telemetry state:
    /// 1. Swaps in new tracer provider and updates the hot-reload handle
    /// 2. Updates trace context propagation configuration
    /// 3. Swaps in new meter providers for metrics collection
    /// 4. Updates logging format layer
    /// 5. Stores Prometheus registry for future endpoint creation
    ///
    /// Old providers are safely shut down in blocking tasks to avoid deadlocking the async runtime.
    ///
    /// This method cannot not fail - by the time we reach activation, all plugins have been
    /// successfully initialized and we are committed to applying the new configuration.
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
            // Build a new tracer from the provider and hot-swap it into the tracing subscriber
            let tracer = tracer_provider.tracer_with_scope(
                InstrumentationScope::builder(GLOBAL_TRACER_NAME)
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .build(),
            );
            hot_tracer.reload(tracer);

            // Install the new provider globally and safely drop the old one in a blocking task
            let last_provider = opentelemetry::global::set_tracer_provider(tracer_provider);
            spawn_blocking(move || {
                drop(last_provider);
            });
        }
    }

    /// Reloads metrics providers, installing new ones and storing the old ones for safe shutdown on drop.
    ///
    /// This performs an atomic swap: new providers are installed and old providers are stored back
    /// in `self.new_meter_providers`. The old providers will be safely dropped when this `Activation`
    /// is dropped (using blocking tasks to avoid runtime deadlocks).
    pub(crate) fn reload_metrics(&mut self) {
        let global_meter_provider = meter_provider_internal();
        // Swap new meter providers with old ones. Old providers stored here will be
        // safely dropped in the Drop implementation using blocking tasks.
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

/// Safely drops OpenTelemetry providers using blocking tasks (Phase 3 of reload lifecycle).
///
/// OpenTelemetry providers perform blocking I/O during shutdown (flushing buffers, closing connections).
/// If dropped on an async runtime thread, this can deadlock the runtime. This Drop implementation ensures
/// all providers are moved to blocking tasks for safe cleanup.
///
/// This runs in two scenarios:
/// 1. **After commit**: Drops the old providers that were replaced
/// 2. **If preparation fails**: Drops the new providers that were never activated
impl Drop for Activation {
    fn drop(&mut self) {
        // Drop all meter providers in blocking tasks to avoid runtime deadlocks
        for meter_provider in std::mem::take(&mut self.new_meter_providers).into_values() {
            spawn_blocking(move || drop(meter_provider));
        }

        // Drop tracer provider in blocking task if present
        if let Some(tracer_provider) = self.new_trace_provider.take() {
            spawn_blocking(move || drop(tracer_provider));
        }
    }
}
