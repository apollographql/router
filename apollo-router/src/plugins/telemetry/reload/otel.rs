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
use std::time::Duration;

use anyhow::anyhow;
use once_cell::sync::OnceCell;
use opentelemetry::Context;
use opentelemetry::InstrumentationScope;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::IdGenerator;
use opentelemetry_sdk::trace::Tracer;
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

#[derive(Clone, Debug)]
pub(crate) enum SampledSpan {
    /// The span isn't sampled, so nothing is ever exported for it. `trace_id`/`span_id`
    /// are fabricated purely for local log correlation and never need to match anything.
    NotSampled(TraceId, SpanId),
    /// The span is sampled.
    Sampled,
}

impl SampledSpan {
    pub(crate) fn trace_and_span_id(&self) -> Option<(TraceId, SpanId)> {
        match self {
            SampledSpan::NotSampled(trace_id, span_id) => Some((trace_id.clone(), *span_id)),
            SampledSpan::Sampled => None,
        }
    }
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
        // if this extension is set, that means the parent span was accepted, and so the
        // entire trace is accepted
        self.extensions()
            .get::<SampledSpan>()
            .is_some_and(|s| matches!(s, SampledSpan::Sampled))
    }

    fn get_trace_id(&self) -> Option<TraceId> {
        let extensions = self.extensions();
        extensions
            .get::<SampledSpan>()
            .and_then(|s| s.trace_and_span_id())
            .map(|(trace_id, _)| trace_id)
    }
}
