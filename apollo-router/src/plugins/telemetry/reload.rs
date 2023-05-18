use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry::trace::SamplingDecision;
use opentelemetry::trace::SamplingResult;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TracerProvider;
use tower::BoxError;
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_opentelemetry::OtelData;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::layer::Filter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

use crate::plugins::telemetry::formatters::filter_metric_events;
use crate::plugins::telemetry::formatters::text::TextFormatter;
use crate::plugins::telemetry::formatters::FilteringFormatter;
use crate::plugins::telemetry::metrics;
use crate::plugins::telemetry::metrics::layer::MetricsLayer;
use crate::plugins::telemetry::metrics::{
    METRIC_PREFIX_COUNTER, METRIC_PREFIX_HISTOGRAM, METRIC_PREFIX_MONOTONIC_COUNTER,
    METRIC_PREFIX_VALUE,
};
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

impl SamplingFilter {
    pub(crate) fn new() -> Self {
        Self {}
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
        // Sampling and Otel is a complex topic. Let's break this down.
        // The desire is to have complete traces if they were deemed to be sampled at the root span. The root span can either be local or remote.
        // How is a root span detected? It is NOT just if the span has a parent. Otel makes a sampling decision and stores this in the otel context which is stored in the tracing context.
        //
        // The otel context can be created in one of two ways:
        // 1. If the root span is remote then in `PropagatingMakeSpan` we set up a context (this happens in our axum code).
        // 2. If the root span is not remote we asl the otel sampler if we should sample or not and create a context (this happens in the otel tracing layer).
        // The layer doesn't know if the tracer will actually export the span or not, so it is important that
        // if there are no exporters set then the configured sampler should be always_off. Otherwise otel will go through the process of creating spans even thought they will never be exported.
        //
        // There is another optimisation that we can do, that does make a bit of difference.
        // We can detect at the filter level if the trace is being sampled or not and early abort telling otel if it is.
        // The root span must always be supplied to the otel layer, as it will make the sampling decision and set up the root otel context.
        // This is slightly less efficient that completely presampling, but is lest complex to implement and more sympathetic to the existing otel code.

        if meta.is_span() {
            if let Some(current_span) = cx.lookup_current() {
                if let Some(otel_data) = current_span.extensions().get::<OtelData>() {
                    let otel_context = &otel_data.parent_cx;
                    let span_ref = otel_context.span();
                    let otel_span_context = span_ref.span_context();
                    let is_sampled = otel_span_context.is_sampled();

                    let builder_sample = matches!(
                        otel_data.builder.sampling_result,
                        Some(SamplingResult {
                            decision: SamplingDecision::RecordAndSample,
                            ..
                        })
                    );
                    // If we have got here then we know the span is not a root span.
                    // The sampling decision has two possible sources:
                    // 1. The builder sampling result. This contains the result of the sampler for the root span.
                    // 2. The otel span context. This contains the result of the sampler for the parent span.
                    // The root span context is never valid, so children of root spans will always use the builder sampling result.
                    // Their ancestors will always use the otel span context is_sampled.
                    let decision = is_sampled || builder_sample;
                    return decision;
                }
            }
            // If we get here it's because the span is a root. It should go to the otel layer to make the sampling decision.
        } else if meta.is_event() {
            // If this is an event then let it through if it is not a metric event
            for field in meta.fields() {
                let field_name = field.name();
                if field_name.starts_with(METRIC_PREFIX_MONOTONIC_COUNTER)
                    || field_name.starts_with(METRIC_PREFIX_HISTOGRAM)
                    || field_name.starts_with(METRIC_PREFIX_COUNTER)
                    || field_name.starts_with(METRIC_PREFIX_VALUE)
                {
                    return false;
                }
            }
        }
        return true;
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
