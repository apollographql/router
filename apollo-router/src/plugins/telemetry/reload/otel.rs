//! OpenTelemetry global state management
//!
//! This module manages the global OpenTelemetry state and tracing subscriber initialization.
//! It provides the foundation for hot-reloading telemetry configuration without restarting the router.
//!
//! ## Global State
//!
//! OpenTelemetry requires global state for tracer providers and propagators. This module maintains:
//! - **Tracer handle** ([`OPENTELEMETRY_TRACER_HANDLE`]) - Allows hot-swapping the active tracer
//! - **Format layer handle** ([`FMT_LAYER_HANDLE`]) - Allows hot-swapping the logging format
//!
//! These handles are set once during initialization and then used to reload components when
//! configuration changes.
//!
//! ## Initialization
//!
//! The [`init_telemetry`] function sets up the tracing subscriber stack with:
//! - Dynamic attribute layer for request-scoped attributes
//! - OpenTelemetry layer for distributed tracing
//! - Format layer for structured logging (JSON or text based on TTY)
//! - Rate limiting layer for OpenTelemetry internal log messages
//! - Environment filter for log level control
//!
//! ## Reloading
//!
//! The reload handles enable the activation phase to update telemetry without recreating the
//! entire subscriber stack, which would require restarting the application.

use std::io::IsTerminal;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::anyhow;
use once_cell::sync::OnceCell;
use opentelemetry::Context;
use opentelemetry::InstrumentationScope;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::IdGenerator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::trace::Tracer;
use parking_lot::Mutex;
use tower::BoxError;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::SpanRef;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;

use crate::plugins::telemetry::dynamic_attribute::DynAttributeLayer;
use crate::plugins::telemetry::fmt_layer::FmtLayer;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::otel;
use crate::plugins::telemetry::otel::OpenTelemetryLayer;
use crate::plugins::telemetry::otel::OtelData;
use crate::plugins::telemetry::reload::rate_limit::RateLimitLayer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;
use crate::tracer::TraceId;

pub(crate) type LayeredRegistry = Layered<DynAttributeLayer, Registry>;
pub(in crate::plugins::telemetry) type LayeredTracer =
    Layered<OpenTelemetryLayer<LayeredRegistry, ReloadTracer<Tracer>>, LayeredRegistry>;

/// Global handle for hot-reloading the OpenTelemetry tracer
///
/// This handle allows the activation phase to swap in a new tracer without rebuilding
/// the entire tracing subscriber stack.
pub(in crate::plugins::telemetry) static OPENTELEMETRY_TRACER_HANDLE: OnceCell<
    ReloadTracer<opentelemetry_sdk::trace::Tracer>,
> = OnceCell::new();

/// Global handle for hot-reloading the logging format layer
///
/// This handle allows the activation phase to change logging format (e.g., JSON vs text)
/// without rebuilding the entire tracing subscriber stack.
static FMT_LAYER_HANDLE: OnceCell<
    Handle<Box<dyn Layer<LayeredTracer> + Send + Sync>, LayeredTracer>,
> = OnceCell::new();

/// The tracer provider installed by the most recent activation.
///
/// Nothing else in the process can shut this provider down. `SdkTracer` holds a *strong*
/// `SdkTracerProvider`, and [`OPENTELEMETRY_TRACER_HANDLE`] — a process-lifetime static — holds a
/// tracer via `ReloadTracer`. So `global::set_tracer_provider` only ever drops one of several
/// clones and never triggers the provider's own `Drop` -> `shutdown()`. The consequences are that
/// spans buffered in its batch processors are silently lost at exit, and that those processors'
/// background workers are still polling Tokio timers when the runtime is torn down, which panics.
///
/// Keeping an explicit handle here lets shutdown call
/// [`shutdown_installed_tracer_provider`] instead of relying on drop order.
static INSTALLED_TRACER_PROVIDER: LazyLock<Mutex<Option<SdkTracerProvider>>> =
    LazyLock::new(Default::default);

pub(crate) fn init_telemetry(log_level: &str) -> anyhow::Result<()> {
    let hot_tracer = ReloadTracer::new(
        opentelemetry_sdk::trace::SdkTracerProvider::default()
            .tracer_with_scope(InstrumentationScope::builder("noop").build()),
    );
    let opentelemetry_layer = otel::layer().with_tracer(hot_tracer.clone());

    // We choose json or plain based on tty
    let fmt = if std::io::stdout().is_terminal() {
        FmtLayer::new(Text::default(), std::io::stdout).boxed()
    } else {
        FmtLayer::new(Json::default(), std::io::stdout).boxed()
    };

    let (fmt_layer, fmt_handle) = tracing_subscriber::reload::Layer::new(fmt);

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
            // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
            // filter opentelemetry internal logs to warn level (OTel 0.31 emits INFO logs for provider setup)
            let log_level = format!("{log_level},salsa=error,opentelemetry=warn");
            tracing::debug!("Running the router with log level set to {log_level}");
            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(DynAttributeLayer::new())
                .with(opentelemetry_layer)
                .with(fmt_layer)
                // Rate limit OpenTelemetry internal log messages to avoid log spam when things go wrong
                .with(RateLimitLayer::new(
                    "opentelemetry",
                    Duration::from_secs(10),
                ))
                .with(EnvFilter::try_new(log_level)?)
                .try_init()?;

            Ok(hot_tracer)
        })
        .map_err(|e: BoxError| anyhow!("failed to set OpenTelemetry tracer: {e}"))?;
    FMT_LAYER_HANDLE
        .set(fmt_handle)
        .map_err(|_| anyhow!("failed to set fmt layer handle"))?;

    Ok(())
}

pub(in crate::plugins::telemetry) fn reload_fmt(
    layer: Box<dyn Layer<LayeredTracer> + Send + Sync>,
) {
    if let Some(handle) = FMT_LAYER_HANDLE.get() {
        handle.reload(layer).expect("fmt layer reload must succeed");
    }
}

pub(crate) fn apollo_opentelemetry_initialized() -> bool {
    OPENTELEMETRY_TRACER_HANDLE.get().is_some()
}

/// Records the tracer provider that has just been installed globally, returning the one it
/// replaced. The caller owns the returned provider and must drop it from a blocking context, as
/// dropping the last clone shuts its span processors down.
///
/// See [`INSTALLED_TRACER_PROVIDER`] for why this bookkeeping is needed.
pub(in crate::plugins::telemetry) fn set_installed_tracer_provider(
    provider: SdkTracerProvider,
) -> Option<SdkTracerProvider> {
    INSTALLED_TRACER_PROVIDER.lock().replace(provider)
}

/// Shuts down the tracer provider installed by the last activation, flushing whatever its batch
/// processors have buffered.
///
/// Must be called from a blocking thread, and while the Tokio runtime is still alive: shutdown
/// blocks until each span processor has flushed, and those processors need the runtime to make
/// progress.
pub(crate) fn shutdown_installed_tracer_provider() {
    // Take the provider out before shutting it down: shutdown can emit logs and metrics, which
    // must not re-enter this lock.
    let provider = INSTALLED_TRACER_PROVIDER.lock().take();
    shutdown_tracer_provider(provider);
}

/// Shuts `provider` down, if there is one, and reports failures.
///
/// Calling `shutdown()` rather than dropping the provider guarantees its span processors
/// are flushed and stopped. `SdkTracerProvider` is refcounted, so its `Drop` reaches the
/// processors only when the *last* clone goes away, and the `SdkTracer` in
/// [`OPENTELEMETRY_TRACER_HANDLE`] holds one for the whole process lifetime. `shutdown()` ignores
/// the refcount and reaches the processors regardless.
fn shutdown_tracer_provider(provider: Option<SdkTracerProvider>) {
    let Some(provider) = provider else {
        return;
    };
    if let Err(error) = provider.shutdown() {
        tracing::error!(%error, "Failed to shut down OTel tracer provider cleanly");
    }
}

// When propagating trace headers to a subgraph or coprocessor, we need a valid trace id and span id
// When the SamplingFilter does not sample a trace, those ids are set to 0 and mark the trace as invalid.
// In that case we still need to propagate headers to subgraphs to tell them they should not sample the trace.
// To that end, we update the context just for that request to create valid span et trace ids, with the
// sampling bit set to false
pub(crate) fn prepare_context(context: Context) -> Context {
    if !context.span().span_context().is_valid() {
        // There's no real span behind these ids (the trace isn't sampled), so any
        // random generator works here - nothing needs to match a span build later.
        let id_generator = opentelemetry_sdk::trace::RandomIdGenerator::default();
        let span_context = SpanContext::new(
            id_generator.new_trace_id(),
            id_generator.new_span_id(),
            TraceFlags::default(),
            false,
            TraceState::default(),
        );
        return context.with_remote_span_context(span_context);
    }
    context
}

pub(crate) trait IsSampled {
    fn is_sampled(&self) -> bool;
    fn get_trace_id(&self) -> Option<TraceId>;
}

impl<'a, T> IsSampled for SpanRef<'a, T>
where
    T: tracing_subscriber::registry::LookupSpan<'a>,
{
    fn is_sampled(&self) -> bool {
        self.extensions()
            .get::<OtelData>()
            .is_some_and(|d| d.current_cx.span().is_recording())
    }

    /// Returns the trace ID for this span, or `None` if the span context is invalid.
    ///
    /// An invalid span context (all-zero IDs) is produced by a `NoopTracer` or a not-yet-
    /// initialised provider. Callers should not propagate all-zero IDs into headers or logs.
    fn get_trace_id(&self) -> Option<TraceId> {
        // OtelData is always inserted by on_new_span; the ? is a defensive fallback.
        let extensions = self.extensions();
        let d = extensions.get::<OtelData>()?;
        let otel_span = d.current_cx.span();
        let sc = otel_span.span_context();
        sc.is_valid().then(|| sc.trace_id().to_bytes().into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use opentelemetry::trace::Span as _;
    use opentelemetry::trace::Tracer as _;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanProcessor;

    use super::*;

    /// Records whether the provider propagated a shutdown down to its span processors.
    #[derive(Debug, Default, Clone)]
    struct ShutdownProbe {
        shut_down: Arc<AtomicBool>,
    }

    impl ShutdownProbe {
        fn was_shut_down(&self) -> bool {
            self.shut_down.load(Ordering::SeqCst)
        }
    }

    impl SpanProcessor for ShutdownProbe {
        fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &Context) {}

        fn on_end(&self, _span: SpanData) {}

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            self.shut_down.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The provider a batch processor belongs to used to be shut down only by dropping its last
    /// clone — which never happened, because `ReloadTracer` holds a `SdkTracer` (and therefore a
    /// strong `SdkTracerProvider`) for the whole process lifetime. Spans buffered at exit were
    /// lost, and the processors' background workers outlived the Tokio runtime and panicked.
    #[test]
    fn tracer_provider_is_shut_down_while_a_tracer_still_holds_it() {
        let probe = ShutdownProbe::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(probe.clone())
            .build();

        // Stands in for the tracer stashed in OPENTELEMETRY_TRACER_HANDLE: a strong clone that
        // outlives every attempt to shut the provider down by dropping it.
        let stale_tracer =
            provider.tracer_with_scope(InstrumentationScope::builder("test").build());

        // Sanity check that the probe reports the state we are about to assert a change in, and
        // that merely handing the provider over does not shut it down.
        let provider = Some(provider);
        assert!(!probe.was_shut_down());

        shutdown_tracer_provider(provider);

        assert!(
            probe.was_shut_down(),
            "shutdown must reach the span processors even though `stale_tracer` still holds a \
             strong clone of the provider"
        );
        // A tracer left pointing at the shut-down provider must stop recording rather than hand
        // spans to processors that are no longer running.
        assert!(!stale_tracer.start("after-shutdown").is_recording());
    }

    #[test]
    fn shutting_down_the_tracer_provider_twice_does_not_panic() {
        let probe = ShutdownProbe::default();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(probe.clone())
            .build();

        shutdown_tracer_provider(Some(provider.clone()));
        assert!(probe.was_shut_down());

        shutdown_tracer_provider(Some(provider));
        // And the no-provider-installed case, which is what a second
        // `shutdown_installed_tracer_provider` call sees.
        shutdown_tracer_provider(None);
    }
}
