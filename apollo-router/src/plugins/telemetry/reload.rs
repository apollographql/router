use std::mem;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use rand::thread_rng;
use rand::Rng;
use tower::BoxError;
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::layer::Filter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

use super::tracing::apollo_telemetry::ApolloLayer;
use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::text::TextFormatter;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::metrics::layer::MetricsLayer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;

pub(super) type LayeredTracer = Layered<
    Filtered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, SamplingFilter, Registry>,
    Registry,
>;

// These handles allow hot tracing of layers. They have complex type definitions because tracing has
// generic types in the layer definition.
pub(super) static OPENTELEMETRY_TRACER_HANDLE: OnceCell<
    ReloadTracer<opentelemetry::sdk::trace::Tracer>,
> = OnceCell::new();

static FMT_LAYER_HANDLE: OnceCell<
    Handle<Box<dyn Layer<LayeredTracer> + Send + Sync>, LayeredTracer>,
> = OnceCell::new();

pub(super) static SPAN_SAMPLING_RATE: AtomicU64 =
    AtomicU64::new(unsafe { mem::transmute::<f64, u64>(0.0) });

type FmtReloadLayer =
    tracing_subscriber::reload::Layer<Box<dyn Layer<LayeredTracer> + Send + Sync>, LayeredTracer>;

#[allow(clippy::type_complexity)]
static METRICS_LAYER_HANDLE: OnceCell<
    Handle<MetricsLayer, Layered<FmtReloadLayer, LayeredTracer>>,
> = OnceCell::new();

type MetricsReloadLayer =
    tracing_subscriber::reload::Layer<MetricsLayer, Layered<FmtReloadLayer, LayeredTracer>>;

#[allow(clippy::type_complexity)]
static APOLLO_LAYER_HANDLE: OnceCell<
    Handle<
        Option<ApolloLayer>,
        Layered<MetricsReloadLayer, Layered<FmtReloadLayer, LayeredTracer>>,
    >,
> = OnceCell::new();

pub(crate) fn init_telemetry(log_level: &str) -> Result<()> {
    let hot_tracer = ReloadTracer::new(
        opentelemetry::sdk::trace::TracerProvider::default().versioned_tracer("noop", None, None),
    );
    let opentelemetry_layer = tracing_opentelemetry::layer()
        .with_tracer(hot_tracer.clone())
        .with_filter(SamplingFilter::new());

    // We choose json or plain based on tty
    let fmt = if atty::is(atty::Stream::Stdout) {
        tracing_subscriber::fmt::Layer::new()
            .event_format(FilteringFormatter::new(
                TextFormatter::new()
                    .with_filename(false)
                    .with_line(false)
                    .with_target(false),
                filter_metric_events,
            ))
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
            .boxed()
    };

    let (fmt_layer, fmt_handle) = tracing_subscriber::reload::Layer::new(fmt);

    let (metrics_layer, metrics_handle) =
        tracing_subscriber::reload::Layer::new(MetricsLayer::new(&NoopMeterProvider::default()));

    let (apollo_layer, apollo_handle) = tracing_subscriber::reload::Layer::new(None);

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
            // manually filter salsa logs because some of them run at the INFO level https://github.com/salsa-rs/salsa/issues/425
            let log_level = format!("{log_level},salsa=error");

            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(opentelemetry_layer)
                .with(fmt_layer)
                .with(metrics_layer)
                .with(apollo_layer)
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

    APOLLO_LAYER_HANDLE
        .set(apollo_handle)
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

pub(crate) fn reload_apollo(layer: Option<ApolloLayer>) {
    if let Some(handle) = APOLLO_LAYER_HANDLE.get() {
        handle
            .reload(layer)
            .expect("metrics layer reload must succeed");
    }
}

pub(crate) struct SamplingFilter {}

impl SamplingFilter {
    pub(crate) fn new() -> Self {
        Self {}
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
        let current_span = cx.current_span();

        !meta.is_span()
            || current_span
                .id()
                .map(|id| cx.span(id).is_some())
                .unwrap_or_else(|| self.sample())
    }

    // if the filter enabled the span, then we signal it in the extensions, so it can be looked up by the
    // next span
    fn on_new_span(
        &self,
        _attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        extensions.insert(Sampled);
    }
}

struct Sampled;
