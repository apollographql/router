use std::io::IsTerminal;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TracerProvider;
use opentelemetry_api::trace::SpanContext;
use opentelemetry_api::trace::TraceFlags;
use opentelemetry_api::trace::TraceState;
use opentelemetry_api::Context;
use rand::thread_rng;
use rand::Rng;
use tower::BoxError;
use tracing_core::Subscriber;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::Filter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::registry::SpanRef;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

use super::config::SamplerOption;
use super::config_new::logging::RateLimit;
use super::dynamic_attribute::DynSpanAttributeLayer;
use super::fmt_layer::FmtLayer;
use super::formatters::json::Json;
use super::metrics::span_metrics_exporter::SpanMetricsLayer;
use super::ROUTER_SPAN_NAME;
use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::metrics::layer::MetricsLayer;
use crate::metrics::meter_provider;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::otel;
use crate::plugins::telemetry::otel::OpenTelemetryLayer;
use crate::plugins::telemetry::otel::PreSampledTracer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;
use crate::router_factory::STARTING_SPAN_NAME;

pub(crate) type LayeredRegistry =
    Layered<SpanMetricsLayer, Layered<DynSpanAttributeLayer, Registry>>;

pub(super) type LayeredTracer = Layered<
    Filtered<
        OpenTelemetryLayer<LayeredRegistry, ReloadTracer<Tracer>>,
        SamplingFilter,
        LayeredRegistry,
    >,
    LayeredRegistry,
>;

// These handles allow hot tracing of layers. They have complex type definitions because tracing has
// generic types in the layer definition.
pub(super) static OPENTELEMETRY_TRACER_HANDLE: OnceCell<
    ReloadTracer<opentelemetry::sdk::trace::Tracer>,
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
        opentelemetry::sdk::trace::TracerProvider::default().versioned_tracer(
            "noop",
            None::<String>,
            None::<String>,
            None,
        ),
    );
    let opentelemetry_layer = otel::layer()
        .with_tracer(hot_tracer.clone())
        .with_filter(SamplingFilter::new());

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
                .with(DynSpanAttributeLayer::new())
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
pub(crate) struct SamplingFilter;

#[allow(dead_code)]
impl SamplingFilter {
    pub(crate) fn new() -> Self {
        Self {}
    }

    pub(super) fn configure(sampler: &SamplerOption) {
        let ratio = match sampler {
            SamplerOption::TraceIdRatioBased(ratio) => {
                // can't use std::cmp::min because f64 is not Ord
                if *ratio > 1.0 {
                    1.0
                } else {
                    *ratio
                }
            }
            SamplerOption::Always(s) => match s {
                super::config::Sampler::AlwaysOn => 1f64,
                super::config::Sampler::AlwaysOff => 0f64,
            },
        };

        SPAN_SAMPLING_RATE.store(f64::to_bits(ratio), Ordering::Relaxed);
    }

    fn sample(&self) -> bool {
        let s: f64 = thread_rng().gen_range(0.0..=1.0);
        s <= f64::from_bits(SPAN_SAMPLING_RATE.load(Ordering::Relaxed))
    }
}

impl<S> Filter<S> for SamplingFilter
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn enabled(
        &self,
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        // we ignore metric events
        if !meta.is_span() {
            return meta.fields().iter().any(|f| f.name() == "message");
        }

        // if there's an exsting otel context set by the client request, and it is sampled,
        // then that trace is sampled
        let current_otel_context = opentelemetry::Context::current();
        if current_otel_context.span().span_context().is_sampled() {
            return true;
        }

        let current_span = cx.current_span();
        if let Some(spanref) = current_span
            // the current span, which is the parent of the span that might get enabled here,
            // exists, but it might have been enabled by another layer like metrics
            .id()
            .and_then(|id| cx.span(id))
        {
            return spanref.is_sampled();
        }

        // always sample the router loading trace
        if meta.name() == STARTING_SPAN_NAME {
            return true;
        }

        // we only make the sampling decision on the root span. If we reach here for any other span,
        // it means that the parent span was not enabled, so we should not enable this span either
        if meta.name() != REQUEST_SPAN_NAME && meta.name() != ROUTER_SPAN_NAME {
            return false;
        }

        // - there's no parent span (it's the root), so we make the sampling decision
        self.sample()
    }

    fn on_new_span(
        &self,
        _attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if extensions.get_mut::<SampledSpan>().is_none() {
            extensions.insert(SampledSpan);
        }
    }

    fn on_close(&self, id: tracing_core::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        extensions.remove::<SampledSpan>();
    }
}

struct SampledSpan;

pub(crate) trait IsSampled {
    fn is_sampled(&self) -> bool;
}

impl<'a, T> IsSampled for SpanRef<'a, T>
where
    T: tracing_subscriber::registry::LookupSpan<'a>,
{
    fn is_sampled(&self) -> bool {
        // if this extension is set, that means the parent span was accepted, and so the
        // entire trace is accepted
        let extensions = self.extensions();
        extensions.get::<SampledSpan>().is_some()
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
