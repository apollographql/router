use std::io::IsTerminal;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TracerProvider;
use rand::thread_rng;
use rand::Rng;
use tower::BoxError;
use tracing_core::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
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
use super::metrics::span_metrics_exporter::SpanMetricsLayer;
use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::text::TextFormatter;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::metrics::layer::MetricsLayer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;

pub(crate) type LayeredRegistry = Layered<SpanMetricsLayer, Registry>;

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

#[allow(clippy::type_complexity)]
static METRICS_LAYER_HANDLE: OnceCell<
    Handle<
        MetricsLayer,
        Layered<
            tracing_subscriber::reload::Layer<
                Box<dyn Layer<LayeredTracer> + Send + Sync>,
                LayeredTracer,
            >,
            LayeredTracer,
        >,
    >,
> = OnceCell::new();

static FMT_LAYER_HANDLE: OnceCell<
    Handle<Box<dyn Layer<LayeredTracer> + Send + Sync>, LayeredTracer>,
> = OnceCell::new();

pub(super) static SPAN_SAMPLING_RATE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn init_telemetry(log_level: &str) -> Result<()> {
    let hot_tracer = ReloadTracer::new(
        opentelemetry::sdk::trace::TracerProvider::default().versioned_tracer("noop", None, None),
    );
    let opentelemetry_layer = tracing_opentelemetry::layer()
        .with_tracer(hot_tracer.clone())
        .with_filter(SamplingFilter::new());

    // We choose json or plain based on tty
    let fmt = if std::io::stdout().is_terminal() {
        tracing_subscriber::fmt::Layer::new()
            .event_format(FilteringFormatter::new(
                TextFormatter::new()
                    .with_filename(false)
                    .with_line(false)
                    .with_target(false),
                filter_metric_events,
            ))
            .fmt_fields(NullFieldFormatter)
            .boxed()
    } else {
        tracing_subscriber::fmt::Layer::new()
            .json()
            .map_event_format(|e| {
                FilteringFormatter::new(
                    e.json()
                        .with_current_span(true)
                        .with_span_list(true)
                        .flatten_event(true),
                    filter_metric_events,
                )
            })
            .fmt_fields(NullFieldFormatter)
            .boxed()
    };

    let (fmt_layer, fmt_handle) = tracing_subscriber::reload::Layer::new(fmt);

    let (metrics_layer, metrics_handle) =
        tracing_subscriber::reload::Layer::new(MetricsLayer::new(&NoopMeterProvider::default()));

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
            // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
            let log_level = format!("{log_level},salsa=error");

            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(SpanMetricsLayer::default())
                .with(opentelemetry_layer)
                .with(fmt_layer)
                .with(metrics_layer)
                .with(EnvFilter::try_new(log_level)?)
                .try_init()?;

            Ok(hot_tracer)
        })
        .map_err(|e: BoxError| anyhow!("failed to set OpenTelemetry tracer: {e}"))?;
    METRICS_LAYER_HANDLE
        .set(metrics_handle)
        .map_err(|_| anyhow!("failed to set metrics layer handle"))?;
    FMT_LAYER_HANDLE
        .set(fmt_handle)
        .map_err(|_| anyhow!("failed to set fmt layer handle"))?;

    Ok(())
}

pub(super) fn reload_metrics(layer: MetricsLayer) {
    if let Some(handle) = METRICS_LAYER_HANDLE.get() {
        // If we are now going live with a new controller then maybe stash it.
        metrics::prometheus::commit_new_controller();
        handle
            .reload(layer)
            .expect("metrics layer reload must succeed");
    }
}

pub(super) fn reload_fmt(layer: Box<dyn Layer<LayeredTracer> + Send + Sync>) {
    if let Some(handle) = FMT_LAYER_HANDLE.get() {
        handle.reload(layer).expect("fmt layer reload must succeed");
    }
}

pub(crate) struct SamplingFilter {}

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
        // we ignore events
        if !meta.is_span() {
            return false;
        }

        // if there's an exsting otel context set by the client request, and it is sampled,
        // then that trace is sampled
        let current_otel_context = opentelemetry_api::Context::current();
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

        // we only make the sampling decision on the root span. If we reach here for any other span,
        // it means that the parent span was not enabled, so we should not enable this span either
        if meta.name() != REQUEST_SPAN_NAME {
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
