//! Tower layers that instrument each pipeline stage's service, one submodule per
//! request/response type. Each submodule owns its `Instrument*Layer` type(s) and the
//! `Telemetry::instrument_*_layer` accessor(s) that construct them.

pub(crate) mod connect;
pub(crate) mod connector;
pub(crate) mod execution;
pub(crate) mod http_client;
pub(crate) mod router;
pub(crate) mod subgraph;
pub(crate) mod supergraph;

use super::Telemetry;
use super::apollo_exporter::Sender;

/// What reads the work the telemetry layers do per request, read once for a pipeline as its
/// stacks are assembled, after the telemetry plugin's activation installed the span gate, the
/// meter providers and the Apollo sender its configuration makes (`pipeline::build_pipeline`
/// activates before it assembles).
///
/// A pipeline keeps what it read for its life: a request it serves after the next
/// configuration's activation, while that configuration's pipeline warms up, is instrumented as
/// its own configuration says.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Readers {
    /// Spans are built: the router's subscriber is current and its span gate is open
    /// ([`spans_are_read`](super::reload::span_gate::spans_are_read) lists the gate's readers),
    /// or another subscriber is current, a test's or an embedding application's, which decides
    /// for itself.
    spans: bool,
    /// A measurement reaches a reader: a meter provider has one, a Prometheus or OTLP exporter's,
    /// Apollo's with an Apollo key and graph reference, or a test's.
    metrics: bool,
    /// An event is configured at a level other than `off`.
    events: bool,
    /// Apollo usage reporting is configured, with an Apollo key and graph reference: its sender
    /// sends the reports.
    reporting: bool,
}

impl Readers {
    /// Whether anything reads any of the layers' work.
    fn any(self) -> bool {
        self.spans || self.metrics || self.events || self.reporting
    }

    /// Whether the error counter's work is read: its counts by the error metrics, and its error
    /// events, which are left out of the log lines and which the OpenTelemetry layer attaches to
    /// the span each is emitted in, so by a trace exporter wherever spans are built.
    fn errors(self) -> bool {
        self.metrics || self.spans
    }

    /// Whether a request's router overhead tracker is read, by the overhead histogram and the
    /// router stage's selectors of the overhead, the request's duration and its active subgraph
    /// requests, in an instrument, an event or a span's attribute.
    fn router_overhead(self) -> bool {
        self.metrics || self.events || self.spans
    }
}

impl Telemetry {
    /// Whether usage reporting reads an operation's extended references, the input fields and
    /// enum values it references and the enum values its responses carry: in the extended
    /// reference mode, the default, where Apollo usage reporting is configured, their one reader.
    pub(crate) fn reports_extended_references(&self) -> bool {
        self.readers().reporting
            && matches!(
                self.config.apollo.metrics_reference_mode,
                super::config::ApolloMetricsReferenceMode::Extended
            )
    }

    /// What reads the telemetry layers' work in the pipeline being assembled.
    pub(crate) fn readers(&self) -> Readers {
        Readers {
            spans: super::reload::otel::spans_built()
                || !tracing::dispatcher::get_default(|current| {
                    current.is::<super::reload::span_gate::SpanGate>()
                }),
            metrics: crate::metrics::meter_provider_internal().has_reader(),
            events: self.config.instrumentation.events.any_logged(),
            reporting: matches!(self.apollo_metrics_sender, Sender::Apollo(_)),
        }
    }
}

/// What `make` makes where a reader reads the router's metrics: under the test's own meter
/// provider, which holds one, as a layer is made in a pipeline whose configuration exports its
/// metrics.
#[cfg(all(test, feature = "global-allocator", not(feature = "dhat-heap"), unix))]
pub(crate) fn where_metrics_are_read<T>(make: impl FnOnce() -> T) -> T {
    crate::metrics::test_utils::AGGREGATE_METER_PROVIDER_ASYNC.sync_scope(Default::default(), make)
}

/// The telemetry plugin `configuration` makes, activated under the router's own subscriber as
/// the router activates it at its start, on `runtime`, whose threads the plugin's construction
/// blocks in place on.
#[cfg(all(test, feature = "global-allocator", not(feature = "dhat-heap"), unix))]
pub(crate) fn activated_under_the_routers_subscriber(
    runtime: &tokio::runtime::Runtime,
    configuration: &serde_json::Value,
) -> crate::plugins::test::PluginTestHarness<Telemetry> {
    use crate::plugin::PluginPrivate as _;

    super::reload::otel::init_telemetry("info").expect("the router's subscriber");
    runtime.block_on(async {
        let plugin = crate::plugins::test::PluginTestHarness::<Telemetry>::builder()
            .config(&configuration.to_string())
            .build()
            .await
            .expect("a telemetry plugin");
        plugin.activate();
        plugin
    })
}

/// A telemetry configuration under which nothing reads the telemetry layers' work, with
/// `instruments` as its custom instruments: stdout logging off and no exporter, event or Apollo
/// key.
#[cfg(all(test, feature = "global-allocator", not(feature = "dhat-heap"), unix))]
pub(crate) fn nothing_reads(instruments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"telemetry": {
        "exporters": {"logging": {"stdout": {"enabled": false}}},
        "instrumentation": {"instruments": instruments}
    }})
}
