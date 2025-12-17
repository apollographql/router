use std::fmt::Debug;
use std::time::Duration;
use std::time::Instant;
use dashmap::DashMap;
use tracing_core::field::Visit;
use tracing_core::metadata::Level;
use tracing_core::Event;
use tracing_core::Field;
use tracing_core::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum ErrorType {
    Trace,
    Metric,
    Other,
}

pub(super) struct OtelErrorLayer {
    last_logged: DashMap<ErrorType, Instant>,
}

impl OtelErrorLayer {
    pub(super) fn new() -> Self {
        Self {
            last_logged: DashMap::new(),
        }
    }

    // Allow for map injection to avoid using global map in tests
    #[cfg(test)]
    fn with_map(last_logged: DashMap<ErrorType, Instant>) -> Self {
        Self { last_logged }
    }

    fn threshold() -> Duration {
        #[cfg(test)]
        {
            Duration::from_millis(100)
        }
        #[cfg(not(test))]
        {
            Duration::from_secs(10)
        }
    }

    fn classify(&self, target: &str, msg: &str) -> ErrorType {
        if target.contains("metrics") || msg.contains("Metrics error:") {
            ErrorType::Metric
        } else if target.contains("trace") {
            ErrorType::Trace
        } else {
            ErrorType::Other
        }
    }

    fn message_prefix(level: Level, error_type: ErrorType) -> Option<String> {
        let severity_str = match level {
            Level::ERROR => "error",
            Level::WARN => "warning",
            _ => return None,
        };

        let kind_str = match error_type {
            ErrorType::Trace => "trace",
            ErrorType::Metric => "metric",
            ErrorType::Other => "",
        };

        Some(if kind_str.is_empty() {
            format!("OpenTelemetry {severity_str} occurred")
        } else {
            format!("OpenTelemetry {kind_str} {severity_str} occurred")
        })
    }

    fn should_log(&self, error_type: ErrorType) -> bool {
        let now = Instant::now();
        let threshold = Self::threshold();

        let last_logged = *self
            .last_logged
            .entry(error_type)
            .and_modify(|last| {
                if last.elapsed() > threshold {
                    *last = now;
                }
            })
            .or_insert(now);

        last_logged == now
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"))
        }
    }
}

impl<S> Layer<S> for OtelErrorLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with("opentelemetry") {
            return;
        }
        let level = *meta.level();
        match *meta.level() {
            Level::ERROR | Level::WARN => {}
            _ => return,
        }

        // Pull message string out of trace event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let Some(msg) = visitor.message else {
            return;
        };

        let error_type = self.classify(meta.target(), &msg);

        // Keep track of the number of cardinality overflow errors otel emits. This can be removed
        // after we introduce a way for users to configure custom cardinality limits.
        if msg.contains("Warning: Maximum data points for metric stream exceeded.") {
            u64_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                "A count of how often a telemetry metric hit the hard cardinality limit",
                1
            );
        }

        // Rate limit repetitive logs
        if !self.should_log(error_type) {
            return;
        }

        // Emit as router logs detached from spans
        let Some(message_prefix) = Self::message_prefix(level, error_type) else {
            return;
        };
        let full_message = format!("{}: {}", message_prefix, msg);
        let otel_target = meta.target().to_string();
        let name = meta.name().to_string();

        let metadata = match level {
            Level::ERROR => &OTEL_ERROR_METADATA_ERROR,
            Level::WARN => &OTEL_ERROR_METADATA_WARN,
            _ => return,
        };

        let fields = metadata.fields();
        let message_field = fields.field("message").expect("message field must exist");
        let otel_target_field = fields
            .field("otel.target")
            .expect("otel.target field must exist");
        let name_field = fields.field("name").expect("name field must exist");
        let values = [
            (&message_field, Some(&full_message as &dyn tracing::Value)),
            (&otel_target_field, Some(&otel_target as &dyn tracing::Value)),
            (&name_field, Some(&name as &dyn tracing::Value)),
        ];
        let value_set = fields.value_set(&values);

        let new_event = Event::new(metadata, &value_set);
        ctx.event(&new_event);
    }
}


/// Re-emits OpenTelemetry internal `tracing` events (targets starting with "opentelemetry") as
/// router logs, while ensuring we only emit a single non-empty `message` field. This fixes a bug
/// in OTel logging where their `tracing` macros emit two values for the `message` field, one being
/// an empty string (https://github.com/tokio-rs/tracing/issues/3195).
///
/// We intentionally only re-emit INFO/DEBUG/TRACE. WARN/ERROR are handled by `OtelErrorLayer`,
/// which adds prefixes, classification and rate-limiting.
pub(super) struct ReemitOtelEventsLayer;

// One metadata+callsite per level so we preserve the original verbosity.
// We construct these events explicitly and record them through `Context::event` to avoid
// re-entering the global dispatcher from within `on_event`.

static OTEL_ERROR_CALLSITE_ERROR: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&OTEL_ERROR_METADATA_ERROR);
static OTEL_ERROR_METADATA_ERROR: tracing_core::Metadata = tracing_core::metadata! {
    name: "otel_internal",
    target: "apollo_router::otel_internal",
    level: Level::ERROR,
    fields: &["message","otel.target", "name"],
    callsite: &OTEL_ERROR_CALLSITE_ERROR,
    kind: tracing_core::metadata::Kind::EVENT,
};


static OTEL_ERROR_CALLSITE_WARN: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&OTEL_ERROR_METADATA_WARN);
static OTEL_ERROR_METADATA_WARN: tracing_core::Metadata = tracing_core::metadata! {
    name: "otel_internal",
    target: "apollo_router::otel_internal",
    level: Level::WARN,
    fields: &["message","otel.target", "name"],
    callsite: &OTEL_ERROR_CALLSITE_WARN,
    kind: tracing_core::metadata::Kind::EVENT,
};

static OTEL_REEMIT_CALLSITE_INFO: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&OTEL_REEMIT_METADATA_INFO);
static OTEL_REEMIT_METADATA_INFO: tracing_core::Metadata = tracing_core::metadata! {
    name: "otel_internal",
    target: "apollo_router::otel_internal",
    level: Level::INFO,
    fields: &["message", "otel.target", "name"],
    callsite: &OTEL_REEMIT_CALLSITE_INFO,
    kind: tracing_core::metadata::Kind::EVENT,
};

static OTEL_REEMIT_CALLSITE_DEBUG: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&OTEL_REEMIT_METADATA_DEBUG);
static OTEL_REEMIT_METADATA_DEBUG: tracing_core::Metadata = tracing_core::metadata! {
    name: "otel_internal",
    target: "apollo_router::otel_internal",
    level: Level::DEBUG,
    fields: &["message", "otel.target", "name"],
    callsite: &OTEL_REEMIT_CALLSITE_DEBUG,
    kind: tracing_core::metadata::Kind::EVENT,
};

static OTEL_REEMIT_CALLSITE_TRACE: tracing_core::callsite::DefaultCallsite =
    tracing_core::callsite::DefaultCallsite::new(&OTEL_REEMIT_METADATA_TRACE);
static OTEL_REEMIT_METADATA_TRACE: tracing_core::Metadata = tracing_core::metadata! {
    name: "otel_internal",
    target: "apollo_router::otel_internal",
    level: Level::TRACE,
    fields: &["message", "otel.target", "name"],
    callsite: &OTEL_REEMIT_CALLSITE_TRACE,
    kind: tracing_core::metadata::Kind::EVENT,
};

impl ReemitOtelEventsLayer {
    fn metadata_for_level(level: &Level) -> Option<&'static tracing_core::Metadata<'static>> {
        match *level {
            Level::INFO => Some(&OTEL_REEMIT_METADATA_INFO),
            Level::DEBUG => Some(&OTEL_REEMIT_METADATA_DEBUG),
            Level::TRACE => Some(&OTEL_REEMIT_METADATA_TRACE),
            _ => None,
        }
    }
}

impl<S: Subscriber> Layer<S> for ReemitOtelEventsLayer {
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();

        // Only rewrite OTel's internal logs.
        if !meta.target().starts_with("opentelemetry") {
            return;
        }

        // Only re-emit INFO/DEBUG/TRACE; WARN/ERROR are handled by `OtelErrorLayer`.
        let Some(metadata) = Self::metadata_for_level(meta.level()) else {
            return;
        };

        // Capture the last non-empty `message` value and ignore empty ones.
        struct CaptureMessage {
            message: Option<String>,
        }

        impl Visit for CaptureMessage {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" && !value.is_empty() {
                    self.message = Some(value.to_string());
                }
            }

            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                if field.name() != "message" {
                    return;
                }
                // OTel's implicit message often records as "\"\"".
                let mut s = format!("{value:?}");
                if s == "\"\"" || s.is_empty() {
                    return;
                }
                // If it's a Debug string literal, remove quotes.
                if let (Some(stripped), true) = (s.strip_prefix('"'), s.ends_with('"')) {
                    if let Some(stripped) = stripped.strip_suffix('"') {
                        s = stripped.to_string();
                    }
                }
                if !s.is_empty() {
                    self.message = Some(s);
                }
            }
        }

        let mut visitor = CaptureMessage { message: None };
        event.record(&mut visitor);

        let Some(message) = visitor.message else {
            return;
        };

        let otel_target = meta.target().to_string();
        let name = meta.name().to_string();
        let fields = metadata.fields();

        let message_field = fields.field("message").expect("message field must exist");
        let otel_target_field = fields
            .field("otel.target")
            .expect("otel.target field must exist");
        let name_field = fields.field("name").expect("name field must exist");
        let values = [
            (&message_field, Some(&message as &dyn tracing::Value)),
            (&otel_target_field, Some(&otel_target as &dyn tracing::Value)),
            (&name_field, Some(&name as &dyn tracing::Value)),
        ];
        let value_set = fields.value_set(&values);

        // Build a real `tracing_core::Event` (callsite is registered via DefaultCallsite),
        // then dispatch through the global dispatcher so EnvFilter still applies.
        let new_event: Event = if let Some(parent) = event.parent().cloned() {
            Event::new_child_of(parent, metadata, &value_set)
        } else {
            Event::new(metadata, &value_set)
        };

        ctx.event(&new_event)
    }
}


#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dashmap::DashMap;
    use tracing_core::Level;
    use serde::Deserialize;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::io;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::Layer;
    use crate::plugins::telemetry::formatters::json::Json;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::fmt_layer::FmtLayer;
    use crate::plugins::telemetry::otel_layers::{OtelErrorLayer, ReemitOtelEventsLayer};

    #[tokio::test]
    async fn test_error_layer_throttles_repeated_messages() {
        let layer = super::OtelErrorLayer::with_map(DashMap::new());
        assert!(
            layer.should_log(super::ErrorType::Metric),
            "first metric error should be logged"
        );
        assert!(
            !layer.should_log(super::ErrorType::Metric),
            "second metric error within threshold should be suppressed"
        );
        // Wait longer than the test threshold (100ms) so the window expires
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            layer.should_log(super::ErrorType::Metric),
            "metric error after threshold should be logged again"
        );
    }

    #[test]
    fn test_message_prefix_error_metric() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::ERROR, super::ErrorType::Metric)
            .expect("prefix should be generated for metric errors");

        assert_eq!(prefix, "OpenTelemetry metric error occurred");
    }

    #[test]
    fn test_message_prefix_error_trace() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::ERROR, super::ErrorType::Trace)
            .expect("prefix should be generated for trace errors");

        assert_eq!(prefix, "OpenTelemetry trace error occurred");
    }

    #[test]
    fn test_message_prefix_error_other() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::ERROR, super::ErrorType::Other)
            .expect("prefix should be generated for generic errors");

        assert_eq!(prefix, "OpenTelemetry error occurred");
    }

    #[test]
    fn test_message_prefix_warn_metric() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::WARN, super::ErrorType::Metric)
            .expect("prefix should be generated for metric warnings");

        assert_eq!(prefix, "OpenTelemetry metric warning occurred");
    }

    #[test]
    fn test_message_prefix_warn_trace() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::WARN, super::ErrorType::Trace)
            .expect("prefix should be generated for trace warnings");

        assert_eq!(prefix, "OpenTelemetry trace warning occurred");
    }

    #[test]
    fn test_message_prefix_warn_other() {
        let prefix = super::OtelErrorLayer::message_prefix(Level::WARN, super::ErrorType::Other)
            .expect("prefix should be generated for generic warnings");

        assert_eq!(prefix, "OpenTelemetry warning occurred");
    }

    #[test]
    fn test_message_prefix_non_error_levels_return_none() {
        assert!(
            super::OtelErrorLayer::message_prefix(Level::INFO, super::ErrorType::Metric,).is_none(),
            "INFO level should not produce a prefix",
        );

        assert!(
            super::OtelErrorLayer::message_prefix(Level::DEBUG, super::ErrorType::Trace,).is_none(),
            "DEBUG level should not produce a prefix",
        );

        assert!(
            super::OtelErrorLayer::message_prefix(Level::TRACE, super::ErrorType::Other,).is_none(),
            "TRACE level should not produce a prefix",
        );
    }

    #[tokio::test]
    async fn test_cardinality_overflow_1() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::registry::Registry;

        async {
            let otel_layer = super::OtelErrorLayer::new();
            let subscriber = Registry::default().with(otel_layer);
            let _guard = tracing::subscriber::set_default(subscriber);

            let msg = "Metrics error: Warning: Maximum data points for metric stream exceeded. \
                   Entry added to overflow. Subsequent overflows to same metric until next \
                   collect will not be logged.";

            tracing::warn!(
                target: "opentelemetry::metrics",
                "{msg}"
            );

            assert_counter!("apollo.router.telemetry.metrics.cardinality_overflow", 1);
        }
            .with_metrics()
            .await;
    }

    #[tokio::test]
    async fn test_cardinality_overflow_2() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::registry::Registry;

        async {
            let otel_layer = super::OtelErrorLayer::new();
            let subscriber = Registry::default().with(otel_layer);
            let _guard = tracing::subscriber::set_default(subscriber);

            let msg = "Warning: Maximum data points for metric stream exceeded. Entry added to overflow.";

            tracing::warn!(
                target: "opentelemetry::metrics",
                "{msg}"
            );

            assert_counter!("apollo.router.telemetry.metrics.cardinality_overflow", 1);
        }
            .with_metrics()
            .await;
    }

    #[derive(Clone)]
    struct BufMakeWriter(Arc<Mutex<Vec<u8>>>);

    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufMakeWriter {
        type Writer = BufWriter;

        fn make_writer(&'a self) -> Self::Writer {
            BufWriter(self.0.clone())
        }
    }

    impl io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut locked = self.0.lock().expect("lock");
            locked.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Deserialize)]
    struct LogLine {
        message: String,

        #[serde(flatten)]
        rest: HashMap<String, Value>,
    }

    fn take_lines(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<String> {
        let bytes = std::mem::take(&mut *buf.lock().expect("lock"));
        let s = String::from_utf8(bytes).expect("utf8");
        s.lines()
            .map(|l| l.to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    fn otel_error_layer_reemits_metric_warn_as_router_log() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                target: "opentelemetry::metrics",
                Level::WARN,
                message = "Warning: Maximum data points for metric stream exceeded."
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(
            parsed.message,
            "OpenTelemetry metric warning occurred: Warning: Maximum data points for metric stream exceeded."
        );
        assert_eq!(
            parsed.rest.get("otel.target").and_then(|v| v.as_str()),
            Some("opentelemetry::metrics"),
            "OtelErrorLayer output should include the original OTel target"
        );
    }

    #[test]
    fn otel_error_layer_reemits_trace_error_as_router_log() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
                target: "opentelemetry_sdk::trace::span_processor",
                Level::ERROR,
                message = "export failed"
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(parsed.message, "OpenTelemetry trace error occurred: export failed");
        assert_eq!(
            parsed.rest.get("otel.target").and_then(|v| v.as_str()),
            Some("opentelemetry_sdk::trace::span_processor")
        );
    }

    #[test]
    fn otel_error_layer_classifies_metric_by_message_when_target_is_generic() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
            target: "opentelemetry_sdk::something",
            Level::WARN,
            message = "Metrics error: boom"
        );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(parsed.message, "OpenTelemetry metric warning occurred: Metrics error: boom");
    }

    #[test]
    fn otel_error_layer_ignores_info_level_events() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        // Filter out raw OTel targets; INFO isn't re-emitted by OtelErrorLayer, so expect no output.
        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(
            target: "opentelemetry::metrics",
            Level::INFO,
            message = "info should be ignored"
        );
        });

        let lines = take_lines(&buf);
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn otel_error_layer_rate_limits_per_error_type_end_to_end() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        // Important: use `set_default` (not `with_default`) so the subscriber stays installed across await.
        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);
        let _guard = tracing::subscriber::set_default(subscriber);

        tracing::event!(
            target: "opentelemetry::metrics",
            Level::WARN,
            message = "metric message 1 should emit"
        );
        tracing::event!(
            target: "opentelemetry::metrics",
            Level::WARN,
            message = "metric message 2 should be suppressed"
        );
        tracing::event!(
            target: "opentelemetry_sdk::trace::span_processor",
            Level::WARN,
            message = "trace message 1 should emit"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;

        // After window -> metric emits again.
        tracing::event!(
        target: "opentelemetry::metrics",
        Level::WARN,
        message = "metric message 3 should emit"
    );

        drop(_guard);

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 3);

        let mut msgs = lines
            .iter()
            .map(|l| serde_json::from_str::<LogLine>(l).expect("valid JSON").message)
            .collect::<Vec<_>>();
        msgs.sort();

        assert_eq!(msgs[0], "OpenTelemetry metric warning occurred: metric message 1 should emit");
        assert_eq!(msgs[1], "OpenTelemetry metric warning occurred: metric message 3 should emit");
        assert_eq!(msgs[2], "OpenTelemetry trace warning occurred: trace message 1 should emit");
    }

    #[test]
    fn otel_error_layer_ignores_non_opentelemetry_targets_end_to_end() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        // Only allow router-internal otel logs through this formatter (isolates OtelErrorLayer output).
        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                meta.target() == "apollo_router::otel_internal"
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "not_otel", "hello");
        });

        let lines = take_lines(&buf);
        assert!(lines.is_empty());
    }

    #[test]
    fn reemits_otel_event_with_single_non_empty_message() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        // Match production: hide raw OTel targets from the formatter.
        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            // This provides two message fields, the explicitly defined `"Last reference dropped"` and
            // the implicitly defined `""` empty string. [oai_citation:2‡Docs.rs](https://docs.rs/tracing/latest/tracing/macro.event.html)
            tracing::info!(
                target: "opentelemetry_sdk::metrics::registry",
                message = "Last reference dropped",
                ""
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1, "expected exactly one re-emitted line");

        // If there are duplicate `message` keys, derived Deserialize fails with a duplicate-field error.  [oai_citation:3‡Docs.rs](https://docs.rs/serde/latest/serde/de/trait.Error.html?utm_source=chatgpt.com)
        let parsed: LogLine =
            serde_json::from_str(&lines[0]).expect("valid JSON without duplicate keys");

        assert_eq!(parsed.message, "Last reference dropped");
        assert_eq!(
            parsed.rest.get("otel.target").and_then(|v| v.as_str()),
            Some("opentelemetry_sdk::metrics::registry")
        );
    }

    #[test]
    fn does_not_reemit_when_only_empty_message_is_present() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer =
            FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
                !meta.target().starts_with("opentelemetry")
            }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "opentelemetry_sdk::metrics::registry",
                ""
            );
        });

        let lines = take_lines(&buf);
        assert!(
            lines.is_empty(),
            "expected no output for empty-message-only OTel event"
        );
    }

    #[test]
    fn reemits_otel_event_when_only_implicit_message_is_present() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer = FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
            !meta.target().starts_with("opentelemetry")
        }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "opentelemetry_sdk::metrics::registry",
                "Implicit message only"
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(parsed.message, "Implicit message only");
        assert_eq!(
            parsed.rest.get("otel.target").and_then(|v| v.as_str()),
            Some("opentelemetry_sdk::metrics::registry")
        );
    }

    #[test]
    fn reemits_otel_event_prefers_implicit_message_when_explicit_message_is_empty() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer = FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
            !meta.target().starts_with("opentelemetry")
        }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "opentelemetry_sdk::metrics::registry",
                message = "", // This should be ignored
                "This message should be reemitted"
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(parsed.message, "This message should be reemitted");
    }

    #[test]
    fn reemits_otel_debug_and_trace_events() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer = FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
            !meta.target().starts_with("opentelemetry")
        }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "opentelemetry_sdk::metrics::registry",
                message = "debug message",
                ""
            );
            tracing::trace!(
                target: "opentelemetry_sdk::metrics::registry",
                message = "trace message",
                ""
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 2);

        let p0: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        let p1: LogLine = serde_json::from_str(&lines[1]).expect("valid JSON");

        assert!(
            (p0.message == "debug message" && p1.message == "trace message")
                || (p0.message == "trace message" && p1.message == "debug message")
        );
    }

    #[test]
    fn warn_is_handled_by_otel_error_layer_not_reemit_layer() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        let fmt_layer = FmtLayer::new(Json::default(), make_writer).with_filter(filter_fn(|meta| {
            !meta.target().starts_with("opentelemetry")
        }));

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: "opentelemetry::metrics",
                "Metrics error: Warning: Maximum data points for metric stream exceeded."
            );
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert!(parsed.message.contains("OpenTelemetry") && parsed.message.contains("warning"));
        assert_eq!(
            parsed.rest.get("otel.target").and_then(|v| v.as_str()),
            Some("opentelemetry::metrics")
        );
    }

    #[test]
    fn does_not_reemit_non_otel_targets() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let make_writer = BufMakeWriter(buf.clone());

        // No target filter: we want to see the original event.
        let fmt_layer = FmtLayer::new(Json::default(), make_writer);

        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(OtelErrorLayer::new())
            .with(ReemitOtelEventsLayer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "not_otel", "hello");
        });

        let lines = take_lines(&buf);
        assert_eq!(lines.len(), 1);

        let parsed: LogLine = serde_json::from_str(&lines[0]).expect("valid JSON");
        assert_eq!(parsed.message, "hello");
        assert!(!parsed.rest.contains_key("otel.target"));
    }
}
