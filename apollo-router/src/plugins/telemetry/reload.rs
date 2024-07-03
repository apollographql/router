use std::io::IsTerminal;
use std::sync::atomic::AtomicU64;

use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry::Context;
use opentelemetry_sdk::trace::Tracer;
use tower::BoxError;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::SpanRef;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

use super::config_new::logging::RateLimit;
use super::dynamic_attribute::DynAttributeLayer;
use super::fmt_layer::FmtLayer;
use super::formatters::json::Json;
use super::metrics::span_metrics_exporter::SpanMetricsLayer;
use crate::metrics::layer::MetricsLayer;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::otel;
use crate::plugins::telemetry::otel::OpenTelemetryLayer;
use crate::plugins::telemetry::otel::PreSampledTracer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;
use crate::tracer::TraceId;

pub(crate) type LayeredRegistry = Layered<SpanMetricsLayer, Layered<DynAttributeLayer, Registry>>;

pub(super) type LayeredTracer =
    Layered<OpenTelemetryLayer<LayeredRegistry, ReloadTracer<Tracer>>, LayeredRegistry>;

// These handles allow hot tracing of layers. They have complex type definitions because tracing has
// generic types in the layer definition.
pub(super) static OPENTELEMETRY_TRACER_HANDLE: OnceCell<
    ReloadTracer<opentelemetry_sdk::trace::Tracer>,
> = OnceCell::new();

static FMT_LAYER_HANDLE: OnceCell<
    Handle<Box<dyn Layer<LayeredTracer> + Send + Sync>, LayeredTracer>,
> = OnceCell::new();

pub(super) static SPAN_SAMPLING_RATE: AtomicU64 = AtomicU64::new(0);

pub(super) static METRICS_LAYER: OnceCell<MetricsLayer> = OnceCell::new();
pub(crate) fn metrics_layer() -> &'static MetricsLayer {
    METRICS_LAYER.get_or_init(|| MetricsLayer::new(meter_provider().clone()))
}

pub(crate) fn init_telemetry(log_level: &str) -> Result<()> {
    let hot_tracer = ReloadTracer::new(
        opentelemetry_sdk::trace::TracerProvider::default()
            .tracer_builder("noop")
            .build(),
    );
    let opentelemetry_layer = otel::layer().with_tracer(hot_tracer.clone());

    // We choose json or plain based on tty
    let fmt = if std::io::stdout().is_terminal() {
        FmtLayer::new(
            FilteringFormatter::new(Text::default(), filter_metric_events, &RateLimit::default()),
            std::io::stdout,
        )
        .boxed()
    } else {
        FmtLayer::new(
            FilteringFormatter::new(Json::default(), filter_metric_events, &RateLimit::default()),
            std::io::stdout,
        )
        .boxed()
    };

    let (fmt_layer, fmt_handle) = tracing_subscriber::reload::Layer::new(fmt);

    let metrics_layer = metrics_layer();

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
            // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
            let log_level = format!("{log_level},salsa=error");
            tracing::debug!("Running the router with log level set to {log_level}");
            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(DynAttributeLayer::new())
                .with(SpanMetricsLayer::default())
                .with(opentelemetry_layer)
                .with(fmt_layer)
                .with(metrics_layer.clone())
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

pub(super) fn reload_fmt(layer: Box<dyn Layer<LayeredTracer> + Send + Sync>) {
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
        if let Some(tracer) = OPENTELEMETRY_TRACER_HANDLE.get() {
            let span_context = SpanContext::new(
                tracer.new_trace_id(),
                tracer.new_span_id(),
                TraceFlags::default(),
                false,
                TraceState::default(),
            );
            return context.with_remote_span_context(span_context);
        }
    }
    context
}

#[derive(Clone, Debug)]
pub(crate) enum SampledSpan {
    NotSampled(TraceId, SpanId),
    Sampled(TraceId, SpanId),
}

impl SampledSpan {
    pub(crate) fn trace_and_span_id(&self) -> (TraceId, SpanId) {
        match self {
            SampledSpan::NotSampled(trace_id, span_id)
            | SampledSpan::Sampled(trace_id, span_id) => (trace_id.clone(), *span_id),
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
        let extensions = self.extensions();
        extensions
            .get::<SampledSpan>()
            .map(|s| matches!(s, SampledSpan::Sampled(_, _)))
            .unwrap_or_default()
    }

    fn get_trace_id(&self) -> Option<TraceId> {
        let extensions = self.extensions();
        extensions.get::<SampledSpan>().map(|s| match s {
            SampledSpan::Sampled(trace_id, _) | SampledSpan::NotSampled(trace_id, _) => {
                trace_id.clone()
            }
        })
    }
}
/// prevents span fields from being formatted to a string when writing logs
pub(crate) struct NullFieldFormatter;

impl<'writer> FormatFields<'writer> for NullFieldFormatter {
    fn format_fields<R: tracing_subscriber::prelude::__tracing_subscriber_field_RecordFields>(
        &self,
        _writer: tracing_subscriber::fmt::format::Writer<'writer>,
        _fields: R,
    ) -> std::fmt::Result {
        Ok(())
    }
}
