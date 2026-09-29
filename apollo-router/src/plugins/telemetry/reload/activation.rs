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
//!
//! Dropping a tracer provider is not enough: every tracer and span holds a clone of its provider,
//! and the SDK shuts the provider down when the last clone is dropped. A span that is still being
//! exported when the provider is replaced could otherwise run that shutdown on an async worker.
//! Retired tracer providers are therefore shut down explicitly, after which later drops do nothing.
//!
//! The global OpenTelemetry API does not hand back the provider it replaces, so the router keeps its
//! own [`TracerProviderHandle`]. The router factory owns it for the lifetime of the state machine and
//! supplies it to each telemetry plugin it creates, which passes it to [`Activation::commit()`].
//! Commit swaps the new provider into it and retires the old one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use opentelemetry::InstrumentationScope;
use opentelemetry::propagation::TextMapCompositePropagator;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use parking_lot::Mutex;
use prometheus::Registry;
use tokio::task::block_in_place;
#[cfg(not(test))]
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
    /// The new tracer provider. None means leave the existing one.
    /// After commit this holds the retired provider, which is shut down on drop.
    new_trace_provider: Option<SdkTracerProvider>,

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

/// The tracer provider that the router has installed globally.
///
/// Owned by the router factory, which outlives every router it creates, so a reload that leaves
/// tracing unchanged keeps the installed provider. Clones share the same provider.
#[derive(Clone, Default)]
pub(crate) struct TracerProviderHandle(Arc<Mutex<Option<SdkTracerProvider>>>);

impl TracerProviderHandle {
    /// Records `tracer_provider` as installed and returns the provider it replaces.
    fn replace(&self, tracer_provider: SdkTracerProvider) -> Option<SdkTracerProvider> {
        self.0.lock().replace(tracer_provider)
    }

    /// Shuts down the installed tracer provider, flushing any pending spans.
    ///
    /// This MUST be called from a blocking thread.
    pub(crate) fn shutdown(&self) {
        let tracer_provider = self.0.lock().take();
        if let Some(tracer_provider) = tracer_provider {
            shutdown(tracer_provider);
        }
    }
}

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

    pub(crate) fn with_tracer_provider(&mut self, tracer_provider: SdkTracerProvider) {
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
    /// 1. Swaps in new tracer provider, records it in `installed` and updates the hot-reload handle
    /// 2. Updates trace context propagation configuration
    /// 3. Swaps in new meter providers for metrics collection
    /// 4. Updates logging format layer
    /// 5. Stores Prometheus registry for future endpoint creation
    ///
    /// Old providers are safely shut down in blocking tasks to avoid deadlocking the async runtime.
    ///
    /// This method cannot not fail - by the time we reach activation, all plugins have been
    /// successfully initialized and we are committed to applying the new configuration.
    pub(crate) fn commit(mut self, installed: &TracerProviderHandle) {
        self.reload_tracing(installed);
        self.reload_trace_propagation();
        self.reload_metrics();
        self.reload_logging();
        *REGISTRY.lock() = self.prometheus_registry.clone();
    }

    fn reload_tracing(&mut self, installed: &TracerProviderHandle) {
        // Only apply things if we were executing in the context of a vanilla the Apollo executable.
        // Users that are rolling their own routers will need to set up telemetry themselves.
        if let Some(hot_tracer) = OPENTELEMETRY_TRACER_HANDLE.get()
            && let Some(tracer_provider) = self.swap_installed_tracer_provider(installed)
        {
            // Build a new tracer from the provider and hot-swap it into the tracing subscriber
            let scope = InstrumentationScope::builder(GLOBAL_TRACER_NAME)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build();
            let tracer = tracer_provider.tracer_with_scope(scope);
            hot_tracer.reload(tracer);

            // Install the new provider globally. `set_tracer_provider` drops the provider it
            // replaces rather than returning it. We still hold the retired provider, so that drop
            // cannot trigger its shutdown, but block_in_place keeps the worker safe if the global
            // held the last reference to a provider installed elsewhere.
            block_in_place(move || opentelemetry::global::set_tracer_provider(tracer_provider));
        }
    }

    /// Records the new tracer provider in `installed` and keeps the provider it replaces, so that
    /// Drop shuts the retired provider down on a blocking thread. Returns the new provider for the
    /// caller to install, or None if tracing is unchanged.
    fn swap_installed_tracer_provider(
        &mut self,
        installed: &TracerProviderHandle,
    ) -> Option<SdkTracerProvider> {
        let tracer_provider = self.new_trace_provider.take()?;
        self.new_trace_provider = installed.replace(tracer_provider.clone());
        Some(tracer_provider)
    }

    /// Reloads metrics providers, installing new ones and storing the old ones for safe shutdown on drop.
    ///
    /// This performs an atomic swap: new providers are installed and old providers are stored back
    /// in `self.new_meter_providers`. The old providers will be safely dropped when this `Activation`
    /// is dropped (using blocking tasks to avoid runtime deadlocks).
    ///
    /// Unlike tracer providers, dropping is sufficient here: meters and instruments do not hold
    /// their meter provider, so the stored provider is its last reference.
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
///
/// The tracer provider is shut down explicitly because in-flight spans may still hold clones of it.
impl Drop for Activation {
    fn drop(&mut self) {
        let meter_providers = std::mem::take(&mut self.new_meter_providers);
        let tracer_provider = self.new_trace_provider.take();
        let cleanup = move || {
            drop(meter_providers);
            if let Some(tracer_provider) = tracer_provider {
                shutdown(tracer_provider);
            }
        };

        // In tests, drop providers synchronously via block_in_place. This avoids a race
        // condition between spawn_blocking and Runtime::drop: when the tokio test runtime
        // shuts down, it cancels async tasks (including PeriodicReader background tasks)
        // and then waits indefinitely for blocking tasks. A race in
        // futures_channel::mpsc::Receiver::drop can cause the PeriodicReader's shutdown
        // message to be lost, leaving the blocking task's futures_executor::block_on call
        // stuck forever. By using block_in_place, we shut down providers while the runtime
        // is still fully alive, so background tasks process shutdown messages normally.
        #[cfg(test)]
        {
            block_in_place(cleanup);
        }

        #[cfg(not(test))]
        {
            spawn_blocking(cleanup);
        }
    }
}

/// Shuts down a tracer provider. This blocks until its span processors have shut down, so it MUST
/// be called from a blocking thread.
fn shutdown(tracer_provider: SdkTracerProvider) {
    if let Err(error) = tracer_provider.shutdown() {
        tracing::warn!(%error, "failed to shut down tracer provider");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use opentelemetry::Context;
    use opentelemetry::trace::Tracer;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::Span;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanProcessor;

    use super::*;

    #[derive(Debug)]
    struct CountShutdowns(Arc<AtomicUsize>);

    impl SpanProcessor for CountShutdowns {
        fn on_start(&self, _span: &mut Span, _cx: &Context) {}

        fn on_end(&self, _span: SpanData) {}

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Builds a tracer provider and a count of how many times it has been shut down.
    fn counting_tracer_provider() -> (SdkTracerProvider, Arc<AtomicUsize>) {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let tracer_provider = SdkTracerProvider::builder()
            .with_span_processor(CountShutdowns(shutdowns.clone()))
            .build();
        (tracer_provider, shutdowns)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retired_tracer_provider_is_shut_down_while_a_span_still_holds_it() {
        let (retired, shutdowns) = counting_tracer_provider();
        // A span that straddles the reload keeps its own reference to the retired provider.
        let in_flight_span = retired.tracer("test").start("in-flight");

        // After commit, the retired provider is held in the activation until it is dropped.
        let mut activation = Activation::new();
        activation.new_trace_provider = Some(retired);
        drop(activation);
        assert_eq!(
            shutdowns.load(Ordering::SeqCst),
            1,
            "the retired provider must be shut down by the activation, not by the last span"
        );

        // Dropping the last reference must not run the shutdown again.
        drop(in_flight_span);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn committing_a_tracer_provider_retires_the_installed_one() {
        let (old, old_shutdowns) = counting_tracer_provider();
        let installed = TracerProviderHandle::default();
        assert!(installed.replace(old).is_none());

        let (new, new_shutdowns) = counting_tracer_provider();
        let mut activation = Activation::new();
        activation.with_tracer_provider(new);
        let to_install = activation
            .swap_installed_tracer_provider(&installed)
            .expect("a changed tracing configuration has a provider to install");
        // Stands in for the global, which holds the new provider until the next reload.
        drop(to_install);
        drop(activation);

        assert_eq!(
            old_shutdowns.load(Ordering::SeqCst),
            1,
            "the previously installed provider must be retired"
        );
        assert_eq!(
            new_shutdowns.load(Ordering::SeqCst),
            0,
            "the newly installed provider must stay live"
        );

        // The handle now holds the new provider, which its owner shuts down when the router stops.
        installed.shutdown();
        assert_eq!(new_shutdowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn installed_tracer_provider_outlives_activations_that_do_not_replace_it() {
        let (installed_provider, installed_shutdowns) = counting_tracer_provider();
        let installed = TracerProviderHandle::default();
        assert!(installed.replace(installed_provider).is_none());

        // A reload that leaves tracing unchanged has nothing to swap.
        let mut unchanged = Activation::new();
        assert!(
            unchanged
                .swap_installed_tracer_provider(&installed)
                .is_none()
        );
        drop(unchanged);

        // A failed reload drops its new provider without committing it.
        let (abandoned, abandoned_shutdowns) = counting_tracer_provider();
        let mut failed = Activation::new();
        failed.with_tracer_provider(abandoned);
        drop(failed);
        assert_eq!(abandoned_shutdowns.load(Ordering::SeqCst), 1);

        assert_eq!(
            installed_shutdowns.load(Ordering::SeqCst),
            0,
            "an activation that does not replace the installed provider must not shut it down"
        );

        installed.shutdown();
        assert_eq!(installed_shutdowns.load(Ordering::SeqCst), 1);
    }
}
