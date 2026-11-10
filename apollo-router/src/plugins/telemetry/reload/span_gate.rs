//! Spans built only while something reads them
//!
//! Every span the router opens, on every request, costs the subscriber: a slot in the
//! registry, the OpenTelemetry layer's data, the span fields the logs show, and a callback on
//! every enter and exit. That is work for the readers of spans: an exporter, a propagator, the
//! log lines that print span data, and the configured features that read a request's trace id.
//! With none of them configured, [`SpanGate`] disables every span at its callsite, so a span
//! costs one load of the callsite's cached interest and its fields are never evaluated.
//! [`spans_are_read`] lists the readers.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tracing_core::Interest;
use tracing_core::Metadata;
use tracing_core::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::reload::tracing::TracingConfigurator;

/// Disables every span while it is closed, before the span is made, and leaves events and every
/// other filter's decisions alone.
///
/// The router installs it outermost, so a closed gate stops a span at its callsite: no layer
/// beneath it, the registry included, sees the span. A change of state rebuilds the callsites'
/// cached interest ([`SpanGateHandle::set_read`]).
pub(crate) struct SpanGate {
    open: Arc<AtomicBool>,
}

impl SpanGate {
    /// A gate open from the start, so the spans made before the first configuration's
    /// activation are built, and its handle. `kept_open` keeps it open whatever the
    /// configuration says: the log filter's span-scoped directives read spans.
    pub(crate) fn new(kept_open: bool) -> (Self, SpanGateHandle) {
        let open = Arc::new(AtomicBool::new(true));
        (
            Self { open: open.clone() },
            SpanGateHandle { open, kept_open },
        )
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

impl<S: Subscriber> Layer<S> for SpanGate {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if metadata.is_span() && !self.is_open() {
            Interest::never()
        } else {
            Interest::always()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        !metadata.is_span() || self.is_open()
    }
}

/// Opens and closes a [`SpanGate`].
pub(crate) struct SpanGateHandle {
    open: Arc<AtomicBool>,
    kept_open: bool,
}

impl SpanGateHandle {
    /// Whether the gate lets spans be built.
    pub(crate) fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Opens the gate where `read`, or where it is kept open, and closes it otherwise. A span
    /// made before the change keeps what it is.
    pub(crate) fn set_read(&self, read: bool) {
        let open = read || self.kept_open;
        if self.open.swap(open, Ordering::Relaxed) != open {
            tracing_core::callsite::rebuild_interest_cache();
        }
    }
}

/// Whether anything the telemetry `config` configures reads the router's spans, with
/// `instrumentation` the `telemetry.instrumentation` section of the router's configuration as
/// written. The readers, each of which keeps every span:
///
/// - a trace exporter, OTLP, Datadog or Apollo's, which exports them;
/// - a propagator, which carries a span's context to sources and takes the router span's parent
///   from a request's headers (`Tracing::propagates`, `create_propagator`);
/// - the response trace id, which reads the trace id from the span (`layers/router.rs`,
///   `layers/supergraph.rs`);
/// - a selector or a standard attribute that reads the trace id, the Datadog trace id or the
///   baggage (`config_new::trace_id`, `config_new::get_baggage`), in any instrument, event or
///   span attribute, found as its key in `instrumentation`;
/// - stdout logging whose format prints span data, the span list, the current span, the trace
///   id, the span id or span attributes, which the default formats print
///   ([`Format::prints_span_data`](crate::plugins::telemetry::config_new::logging::Format::prints_span_data));
/// - stdout logging with an event configured, since an event's attributes reach its log line
///   through the span it is logged in (`config_new::events::log_event`).
///
/// The log filter's span-scoped directives read spans too, and keep the gate open from the
/// router's start ([`SpanGate::new`]). A sampler and field-level instrumentation read no span
/// where nothing exports: the FTV1 header's sampling stands in for the span's where spans are
/// not built (`layers/supergraph.rs`, `draw_subgraph_ftv1`).
pub(crate) fn spans_are_read(config: &Conf, instrumentation: Option<&serde_json::Value>) -> bool {
    let tracing = &config.exporters.tracing;
    TracingConfigurator::is_enabled(&tracing.otlp)
        || TracingConfigurator::is_enabled(&tracing.datadog)
        || TracingConfigurator::is_enabled(&config.apollo)
        || tracing.propagates()
        || tracing.response_trace_id.enabled
        || instrumentation.is_some_and(reads_the_trace)
        || config
            .exporters
            .logging
            .stdout
            .format_in_use()
            .is_some_and(|format| {
                format.prints_span_data() || config.instrumentation.events.any_logged()
            })
}

/// Whether `configuration` names, as the key of any of its objects, a selector or a standard
/// attribute reading the trace id or the baggage: `trace_id` (the router selector and the router
/// attribute), `dd.trace_id` (the router attribute) or `baggage` (the router, supergraph and
/// subgraph selectors and the router attribute). A custom attribute named so counts too, which
/// keeps the spans and costs only time.
fn reads_the_trace(configuration: &serde_json::Value) -> bool {
    match configuration {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(key.as_str(), "trace_id" | "dd.trace_id" | "baggage") || reads_the_trace(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(reads_the_trace),
        _ => false,
    }
}
