use anyhow::anyhow;
use anyhow::Result;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::noop::NoopMeterProvider;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use tower::BoxError;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Registry;

use crate::plugins::telemetry::metrics::layer::MetricsLayer;
use crate::plugins::telemetry::tracing::reload::ReloadTracer;

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
                Box<
                    dyn Layer<Layered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, Registry>>
                        + Send
                        + Sync,
                >,
                Layered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, Registry>,
            >,
            Layered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, Registry>,
        >,
    >,
> = OnceCell::new();

#[allow(clippy::type_complexity)]
static FMT_LAYER_HANDLE: OnceCell<
    Handle<
        Box<
            dyn Layer<Layered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, Registry>>
                + Send
                + Sync,
        >,
        Layered<OpenTelemetryLayer<Registry, ReloadTracer<Tracer>>, Registry>,
    >,
> = OnceCell::new();

pub(crate) fn init_telemetry(log_level: &str) -> Result<()> {
    let hot_tracer = ReloadTracer::new(
        opentelemetry::sdk::trace::TracerProvider::default().versioned_tracer("noop", None, None),
    );
    let opentelemetry_layer = tracing_opentelemetry::layer().with_tracer(hot_tracer.clone());

    // We choose json or plain based on tty
    let fmt = if atty::is(atty::Stream::Stdout) {
        tracing_subscriber::fmt::Layer::new()
            .with_target(false)
            .boxed()
    } else {
        tracing_subscriber::fmt::Layer::new().json().boxed()
    };

    let (fmt_layer, fmt_handle) = tracing_subscriber::reload::Layer::new(fmt);

    let (metrics_layer, metrics_handle) =
        tracing_subscriber::reload::Layer::new(MetricsLayer::new(&NoopMeterProvider::default()));

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
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
        handle
            .reload(layer)
            .expect("metrics layer reload must succeed");
    }
}

pub(super) fn reload_tracing(
    layer: Box<
        dyn Layer<
                ::tracing_subscriber::layer::Layered<
                    OpenTelemetryLayer<
                        ::tracing_subscriber::Registry,
                        ReloadTracer<::opentelemetry::sdk::trace::Tracer>,
                    >,
                    ::tracing_subscriber::Registry,
                >,
            > + Send
            + Sync,
    >,
) {
    if let Some(handle) = FMT_LAYER_HANDLE.get() {
        handle.reload(layer).expect("fmt layer reload must succeed");
    }
}
