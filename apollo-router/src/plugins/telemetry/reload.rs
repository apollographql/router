use std::io::IsTerminal;

use anyhow::Result;
use anyhow::anyhow;
use once_cell::sync::OnceCell;
use opentelemetry::Context;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceFlags;
use opentelemetry::trace::TraceState;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::Tracer;
use tower::BoxError;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::SpanRef;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;

use super::dynamic_attribute::DynAttributeLayer;
use super::fmt_layer::FmtLayer;
use super::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::otel;
use crate::plugins::telemetry::otel::OpenTelemetryLayer;
use crate::plugins::telemetry::otel::PreSampledTracer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;
use crate::tracer::TraceId;

pub(crate) type LayeredRegistry = Layered<DynAttributeLayer, Registry>;

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

pub(crate) fn init_telemetry(log_level: &str) -> Result<()> {
    let hot_tracer =
        ReloadTracer::new(opentelemetry_sdk::trace::SdkTracerProvider::default().tracer("noop"));
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
            let log_level = format!("{log_level},salsa=error");
            tracing::debug!("Running the router with log level set to {log_level}");
            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(DynAttributeLayer::new())
                .with(opentelemetry_layer)
                .with(fmt_layer)
                .with(WarnLegacyMetricsLayer)
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
    if !context.span().span_context().is_valid()
        && let Some(tracer) = OPENTELEMETRY_TRACER_HANDLE.get()
    {
        let span_context = SpanContext::new(
            tracer.new_trace_id(),
            tracer.new_span_id(),
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

const LEGACY_METRIC_PREFIX_MONOTONIC_COUNTER: &str = "monotonic_counter.";
const LEGACY_METRIC_PREFIX_COUNTER: &str = "counter.";
const LEGACY_METRIC_PREFIX_HISTOGRAM: &str = "histogram.";
const LEGACY_METRIC_PREFIX_VALUE: &str = "value.";

/// Detects use of the 1.x `tracing`-based metrics events, which are no longer supported in 2.x.
struct WarnLegacyMetricsLayer;

// We can't use the tracing macros inside our `on_event` callback, instead we have to manually
// produce an event, which requires a significant amount of ceremony.
// This metadata mimicks what `tracing::error!()` does.
static WARN_LEGACY_METRIC_CALLSITE: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&WARN_LEGACY_METRIC_METADATA);
static WARN_LEGACY_METRIC_METADATA: tracing_core::Metadata = tracing_core::metadata! {
    name: "warn_legacy_metric",
    target: module_path!(),
    level: tracing_core::Level::ERROR,
    fields: &["message", "metric_name"],
    callsite: &WARN_LEGACY_METRIC_CALLSITE,
    kind: tracing_core::metadata::Kind::EVENT,
};

impl<S: tracing::Subscriber> Layer<S> for WarnLegacyMetricsLayer {
    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        if let Some(field) = event.fields().find(|field| {
            field
                .name()
                .starts_with(LEGACY_METRIC_PREFIX_MONOTONIC_COUNTER)
                || field.name().starts_with(LEGACY_METRIC_PREFIX_COUNTER)
                || field.name().starts_with(LEGACY_METRIC_PREFIX_HISTOGRAM)
                || field.name().starts_with(LEGACY_METRIC_PREFIX_VALUE)
        }) {
            // Doing all this manually is a flippin nightmare!
            // We allocate a bunch but I reckon it's fine because this only happens in a deprecated
            // code path that we want people to upgrade from.
            let fields = WARN_LEGACY_METRIC_METADATA.fields();
            let message_field = fields.field("message").unwrap();
            let message =
                "Detected unsupported legacy metrics reporting, remove or migrate to opentelemetry"
                    .to_string();
            let name_field = fields.field("metric_name").unwrap();
            let metric_name = field.name().to_string();
            let value_set = &[
                (&message_field, Some(&message as &dyn tracing::Value)),
                (&name_field, Some(&metric_name as &dyn tracing::Value)),
            ];
            let value_set = fields.value_set(value_set);
            ctx.event(&tracing_core::Event::new(
                &WARN_LEGACY_METRIC_METADATA,
                &value_set,
            ));
        }
    }
}
