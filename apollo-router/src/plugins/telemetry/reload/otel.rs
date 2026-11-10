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
//! - Format layer for structured logging (JSON or text based on TTY), which only sees events
//! - Span fields layer, which records the span fields the format layer shows
//! - Rate limiting layer for OpenTelemetry internal log messages
//! - Environment filter for log level control, which tracks spans only for span-scoped directives
//! - Span gate, which disables every span while nothing configured reads spans
//!
//! ## Reloading
//!
//! The reload handles enable the activation phase to update telemetry without recreating the
//! entire subscriber stack, which would require restarting the application.

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
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
use tracing_core::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::SpanRef;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;

use crate::plugins::telemetry::dynamic_attribute::DynAttributeLayer;
use crate::plugins::telemetry::fmt_layer::FmtLayer;
use crate::plugins::telemetry::fmt_layer::SpanFieldsLayer;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::otel;
use crate::plugins::telemetry::otel::OpenTelemetryLayer;
use crate::plugins::telemetry::otel::OtelData;
use crate::plugins::telemetry::reload::log_layers::EventsOnly;
use crate::plugins::telemetry::reload::log_layers::LogFilter;
use crate::plugins::telemetry::reload::rate_limit::RateLimitLayer;
use crate::plugins::telemetry::reload::span_gate::SpanGate;
use crate::plugins::telemetry::reload::span_gate::SpanGateHandle;
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
static FMT_LAYER_HANDLE: OnceCell<LogFormatHandle<LayeredTracer>> = OnceCell::new();

/// Global handle for opening and closing the span gate as configurations are activated.
static SPAN_GATE_HANDLE: OnceCell<SpanGateHandle> = OnceCell::new();

/// Swaps the reloadable log format layer, and records span fields only while it prints them.
struct LogFormatHandle<S> {
    layer: Handle<Box<dyn Layer<S> + Send + Sync>, S>,
    /// [`SpanFieldsLayer::enabled`]
    span_fields_enabled: Arc<AtomicBool>,
}

impl<S: Subscriber> LogFormatHandle<S> {
    /// Installs `layer` as the log format layer, or a layer that logs nothing when it is `None`,
    /// recording span fields where `span_fields`, the layer printing them.
    ///
    /// Where nothing prints span fields, they aren't recorded. Logs from a request in flight
    /// when they are printed again therefore lack the fields of spans the request created before
    /// then.
    fn reload(&self, layer: Option<Box<dyn Layer<S> + Send + Sync>>, span_fields: bool) {
        self.span_fields_enabled
            .store(layer.is_some() && span_fields, Ordering::Relaxed);
        self.layer
            .reload(layer.unwrap_or_else(|| NoOpLayer.boxed()))
            .expect("fmt layer reload must succeed");
    }
}

struct NoOpLayer;

impl<S: Subscriber> Layer<S> for NoOpLayer {}

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
    let span_fields = SpanFieldsLayer::default();
    let span_fields_enabled = span_fields.enabled();

    // Stash the reload handles so that we can hot reload later
    OPENTELEMETRY_TRACER_HANDLE
        .get_or_try_init(move || {
            // filter opentelemetry internal logs to warn level (OTel 0.31 emits INFO logs for provider setup)
            let log_level = format!("{log_level},opentelemetry=warn");
            tracing::debug!("Running the router with log level set to {log_level}");
            let log_filter = LogFilter::try_new(&log_level)?;
            let (span_gate, span_gate_handle) = SpanGate::new(log_filter.tracks_spans());
            // Env filter is separate because of https://github.com/tokio-rs/tracing/issues/1629
            // the tracing registry is only created once
            tracing_subscriber::registry()
                .with(DynAttributeLayer::new())
                .with(opentelemetry_layer)
                // The reloadable format layer gets events only, so that span callbacks don't take
                // the reload lock. Span fields for the logs are recorded outside it.
                .with(EventsOnly::new(fmt_layer))
                .with(span_fields)
                // Rate limit OpenTelemetry internal log messages to avoid log spam when things go wrong
                .with(RateLimitLayer::new(
                    "opentelemetry",
                    Duration::from_secs(10),
                ))
                .with(log_filter)
                // Outermost, so that a closed gate stops a span before any layer sees it.
                .with(span_gate)
                .try_init()?;
            let _ = SPAN_GATE_HANDLE.set(span_gate_handle);

            Ok(hot_tracer)
        })
        .map_err(|e: BoxError| anyhow!("failed to set OpenTelemetry tracer: {e}"))?;
    FMT_LAYER_HANDLE
        .set(LogFormatHandle {
            layer: fmt_handle,
            span_fields_enabled,
        })
        .map_err(|_| anyhow!("failed to set fmt layer handle"))?;

    Ok(())
}

/// Installs the router's own subscriber, as [`init_telemetry`] does at the router's start, for a
/// test that reads what the router builds under it: its span gate, its log format.
///
/// The subscriber is the process's global default and its handles are set once, so a second
/// call in one process fails. Such a test relies on its runner giving it a process of its own,
/// as nextest does for every test (`cargo xtask test`, Router's CI); under `cargo test`, which
/// runs a binary's tests in one process, every such test after the first fails here.
#[cfg(test)]
pub(crate) fn install_the_routers_subscriber(log_level: &str) {
    init_telemetry(log_level)
        .expect("the router's subscriber, installed once per process: run the tests under nextest");
}

/// Installs `layer` as the log format layer, recording span fields where `span_fields`; `None`
/// turns stdout logging off.
pub(in crate::plugins::telemetry) fn reload_fmt(
    layer: Option<Box<dyn Layer<LayeredTracer> + Send + Sync>>,
    span_fields: bool,
) {
    if let Some(handle) = FMT_LAYER_HANDLE.get() {
        handle.reload(layer, span_fields);
    }
}

/// Builds the router's spans from now on where `read`, something configured reading them, and
/// disables every span otherwise ([`SpanGate`]).
pub(in crate::plugins::telemetry) fn set_spans_read(read: bool) {
    if let Some(handle) = SPAN_GATE_HANDLE.get() {
        handle.set_read(read);
    }
}

/// Whether the router builds spans: where its subscriber has no span gate, as under a
/// subscriber of a test's or an embedding application's, whatever subscriber is current decides.
pub(crate) fn spans_built() -> bool {
    SPAN_GATE_HANDLE.get().is_none_or(SpanGateHandle::is_open)
}

pub(crate) fn apollo_opentelemetry_initialized() -> bool {
    OPENTELEMETRY_TRACER_HANDLE.get().is_some()
}

/// Records the tracer provider that has just been installed globally, returning the one it
/// replaced. The caller owns the returned provider and must hand it to
/// [`shutdown_tracer_provider`] from a blocking context.
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
/// processors only when the *last* clone goes away, and clones outlive every point at which the
/// router is done with a provider: the `SdkTracer` in [`OPENTELEMETRY_TRACER_HANDLE`] holds one
/// for the whole process lifetime, and every span still in flight at a reload holds one of the
/// provider it started under. `shutdown()` ignores the refcount and reaches the processors
/// regardless.
///
/// Must be called from a blocking thread while the Tokio runtime is still alive: shutting a batch
/// processor down blocks until it has flushed, and it needs the runtime to make progress.
pub(in crate::plugins::telemetry) fn shutdown_tracer_provider(provider: Option<SdkTracerProvider>) {
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
            .is_some_and(|d| d.sampled)
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
    use opentelemetry::trace::Span as _;
    use opentelemetry::trace::Tracer as _;
    use tracing_subscriber::registry::LookupSpan as _;

    use super::*;
    use crate::plugins::telemetry::dynamic_attribute::LogAttributes;
    use crate::plugins::telemetry::reload::testing::ShutdownProbe;

    /// The names of the fields recorded in `span`'s [`LogAttributes`].
    fn log_attribute_names(span: &tracing::Span) -> Vec<String> {
        span.with_subscriber(|(id, dispatch)| {
            let registry = dispatch.downcast_ref::<Registry>().expect("a registry");
            let span = registry.span(id).expect("the span");
            let extensions = span.extensions();
            let attributes = extensions.get::<LogAttributes>().expect("log attributes");
            attributes
                .attributes()
                .iter()
                .map(|kv| kv.key.to_string())
                .collect()
        })
        .expect("a subscriber")
    }

    #[test]
    fn span_fields_are_recorded_only_while_the_log_format_prints_them() {
        let span_fields = SpanFieldsLayer::default();
        let (fmt_layer, layer) = tracing_subscriber::reload::Layer::new(NoOpLayer.boxed());
        let handle = LogFormatHandle {
            layer,
            span_fields_enabled: span_fields.enabled(),
        };
        let subscriber = tracing_subscriber::registry()
            .with(DynAttributeLayer::new())
            .with(EventsOnly::new(fmt_layer))
            .with(span_fields);

        tracing::subscriber::with_default(subscriber, || {
            handle.reload(None, true);
            let off = tracing::info_span!("off", field = "value", late = tracing::field::Empty);
            off.record("late", "value");
            assert!(log_attribute_names(&off).is_empty());

            handle.reload(
                Some(FmtLayer::new(Text::default(), std::io::sink).boxed()),
                false,
            );
            let unprinted =
                tracing::info_span!("unprinted", field = "value", late = tracing::field::Empty);
            unprinted.record("late", "value");
            assert!(log_attribute_names(&unprinted).is_empty());

            handle.reload(
                Some(FmtLayer::new(Text::default(), std::io::sink).boxed()),
                true,
            );
            let on = tracing::info_span!("on", field = "value", late = tracing::field::Empty);
            on.record("late", "value");
            let mut names = log_attribute_names(&on);
            names.sort();
            assert_eq!(names, ["field", "late"]);
            assert!(log_attribute_names(&off).is_empty());
        });
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

    /// A router configuration's `telemetry` section: no exporter, no sampling, no field-level
    /// instrumentation and stdout logging off, so nothing it configures reads a span.
    fn unread() -> serde_json::Value {
        serde_json::json!({"telemetry": {
            "exporters": {
                "tracing": {"common": {"sampler": "always_off"}},
                "logging": {"stdout": {"enabled": false}},
            },
            "apollo": {"field_level_instrumentation_sampler": "always_off"},
        }})
    }

    /// `configuration` with `value` at the JSON pointer `at`, the objects on the way made.
    fn with(
        mut configuration: serde_json::Value,
        at: &str,
        value: serde_json::Value,
    ) -> serde_json::Value {
        let mut place = &mut configuration;
        for key in at.trim_start_matches('/').split('/') {
            place = place
                .as_object_mut()
                .expect("an object on the way")
                .entry(key)
                .or_insert_with(|| serde_json::json!({}));
        }
        *place = value;
        configuration
    }

    /// Whether the router span is built under the subscriber [`init_telemetry`] installed, once
    /// the telemetry plugin `configuration` makes is activated.
    async fn router_span_is_built(configuration: &serde_json::Value) -> bool {
        use crate::plugin::PluginPrivate;
        let plugin = crate::plugins::test::PluginTestHarness::<
            crate::plugins::telemetry::Telemetry,
        >::builder()
        .config(&configuration.to_string())
        .build()
        .await
        .unwrap_or_else(|error| panic!("{configuration}: {error}"));
        plugin.activate();
        let span = crate::plugins::telemetry::span_factory::create_router(&http::Request::new(()));
        !span.is_disabled()
    }

    /// Under the router's own subscriber, a request's spans are disabled at their callsites where
    /// nothing configured reads spans, and stay built wherever something does: an exporter, a
    /// propagator, the response trace id, a selector reading the trace id or baggage, a logged
    /// event or a stdout format printing span data, the default formats among them. With stdout
    /// logging on, the JSON format's span list, trace id and span id off, or the text format's
    /// span list and current span off, and nothing else reading spans, no span is built. A sampler
    /// and field-level instrumentation read no span with nothing exporting. Each reader's row is
    /// followed by a configuration nothing reads spans under, so spans stop being built again
    /// when a reload takes the reader away.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_router_builds_spans_only_where_something_reads_them() {
        install_the_routers_subscriber("info");
        let stdout = |format: serde_json::Value| {
            with(
                unread(),
                "/telemetry/exporters/logging/stdout",
                serde_json::json!({"enabled": true, "format": format}),
            )
        };
        let printing = |format: &str, fields: &[&str]| {
            let mut printed = serde_json::json!({});
            printed[format] = serde_json::json!({
                "display_span_list": false,
                "display_current_span": false,
                "display_trace_id": false,
                "display_span_id": false,
            });
            for field in fields {
                printed[format][field] = true.into();
            }
            stdout(printed)
        };
        let sampling = with(
            unread(),
            "/telemetry/exporters/tracing/common/sampler",
            "always_on".into(),
        );
        let unread_configurations = [
            ("stdout logging off", unread()),
            (
                "the JSON format with its span list, trace id and span id off",
                stdout(serde_json::json!({"json": {
                    "display_span_list": false,
                    "display_trace_id": false,
                    "display_span_id": false,
                }})),
            ),
            (
                "the text format with its span list and current span off",
                stdout(serde_json::json!({"text": {
                    "display_span_list": false,
                    "display_current_span": false,
                }})),
            ),
            (
                "a sampler that samples, with nothing exporting",
                sampling.clone(),
            ),
            (
                "field-level instrumentation, with nothing exporting",
                with(
                    sampling,
                    "/telemetry/apollo/field_level_instrumentation_sampler",
                    "always_on".into(),
                ),
            ),
        ];
        let instrument_attribute = |selector: serde_json::Value| {
            with(
                unread(),
                "/telemetry/instrumentation/instruments/router/acme.requests",
                serde_json::json!({
                    "type": "counter",
                    "value": "unit",
                    "unit": "request",
                    "description": "requests",
                    "attributes": {"read": selector},
                }),
            )
        };
        let readers = [
            (
                "the router's defaults, whose stdout format prints span data",
                serde_json::json!({}),
            ),
            (
                "the JSON format at its defaults: the span list, the trace id and the span id",
                stdout("json".into()),
            ),
            (
                "the text format at its defaults: the span list and the current span",
                stdout("text".into()),
            ),
            (
                "the JSON format's span list",
                printing("json", &["display_span_list"]),
            ),
            (
                "the JSON format's current span",
                printing("json", &["display_current_span"]),
            ),
            (
                "the JSON format's trace id",
                printing("json", &["display_trace_id"]),
            ),
            (
                "the JSON format's span id",
                printing("json", &["display_span_id"]),
            ),
            (
                "the JSON format's span attributes",
                with(
                    printing("json", &[]),
                    "/telemetry/exporters/logging/stdout/format/json/span_attributes",
                    serde_json::json!(["subgraph.name"]),
                ),
            ),
            (
                "the text format's span list",
                printing("text", &["display_span_list"]),
            ),
            (
                "the text format's current span",
                printing("text", &["display_current_span"]),
            ),
            (
                "the text format's trace id",
                printing("text", &["display_trace_id"]),
            ),
            (
                "the text format's span id",
                printing("text", &["display_span_id"]),
            ),
            (
                "a logged event, whose attributes travel through its span",
                with(
                    printing("json", &[]),
                    "/telemetry/instrumentation/events/router/request",
                    "info".into(),
                ),
            ),
            (
                "an OTLP exporter",
                with(
                    unread(),
                    "/telemetry/exporters/tracing/otlp",
                    serde_json::json!({"enabled": true, "endpoint": "http://127.0.0.1:4317"}),
                ),
            ),
            (
                "a Datadog exporter",
                with(
                    unread(),
                    "/telemetry/exporters/tracing/datadog",
                    serde_json::json!({"enabled": true}),
                ),
            ),
            (
                "trace context propagation",
                with(
                    unread(),
                    "/telemetry/exporters/tracing/propagation/trace_context",
                    true.into(),
                ),
            ),
            (
                "a trace id read from a request header",
                with(
                    unread(),
                    "/telemetry/exporters/tracing/propagation/request/header_name",
                    "x-trace-id".into(),
                ),
            ),
            (
                "the response trace id",
                with(
                    unread(),
                    "/telemetry/exporters/tracing/response_trace_id/enabled",
                    true.into(),
                ),
            ),
            (
                "a trace id selector",
                instrument_attribute(serde_json::json!({"trace_id": "open_telemetry"})),
            ),
            (
                "a baggage selector",
                instrument_attribute(serde_json::json!({"baggage": "tenant"})),
            ),
            (
                "the Datadog trace id on the router span",
                with(
                    unread(),
                    "/telemetry/instrumentation/spans/router/attributes/dd.trace_id",
                    true.into(),
                ),
            ),
        ];

        for (configuration, unread) in &unread_configurations {
            assert!(
                !router_span_is_built(unread).await,
                "nothing reads spans under {configuration}, yet they are built"
            );
        }
        for (index, (reader, configuration)) in readers.iter().enumerate() {
            assert!(
                router_span_is_built(configuration).await,
                "spans are read by {reader}, yet not built"
            );
            let (configuration, unread) =
                &unread_configurations[index % unread_configurations.len()];
            assert!(
                !router_span_is_built(unread).await,
                "nothing reads spans under {configuration}, yet they are built after {reader}"
            );
        }
    }

    /// A span-scoped log directive, which enables events only inside a matching span, keeps the
    /// router building spans where its configuration has no other reader.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_span_scoped_log_directive_keeps_spans_built() {
        install_the_routers_subscriber("info,[router]=debug");
        assert!(router_span_is_built(&unread()).await);
    }
}
