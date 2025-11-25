use std::fmt::Debug;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::TryFutureExt;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;
use tracing_core::Event;
use tracing_core::Field;
use tracing_core::Subscriber;
use tracing_core::field::Visit;
use tracing_core::metadata::Level;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

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
        // TODO workshop this
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
            self.message = Some(value.to_owned());
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
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with("opentelemetry") {
            return;
        }
        let level = *meta.level();
        if level < Level::WARN {
            return;
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

        match level {
            Level::ERROR => {
                tracing::error!(
                    parent: None,
                    "{}: {}",
                    message_prefix,
                    msg
                );
            }
            Level::WARN => {
                tracing::warn!(
                    parent: None,
                    "{}: {}",
                    message_prefix,
                    msg
                );
            }
            _ => {}
        }
    }
}

/// Wrapper that modifies trace export errors to include exporter name
pub(crate) struct NamedSpanExporter<E> {
    name: &'static str,
    inner: E,
}

impl<E> NamedSpanExporter<E> {
    pub(crate) fn new(inner: E, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<E: SpanExporter> Debug for NamedSpanExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedSpanExporter")
            .field("name", &self.name)
            .finish()
    }
}

impl<E: SpanExporter> SpanExporter for NamedSpanExporter<E> {
    fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(batch);
        Box::pin(async move {
            fut.await.map_err(|err| {
                let modified = format!("[{} traces] {}", name, err);
                // Recreate as an internal failure to allow us to write a tagged message. This has
                // the unfortunate side effect of removing the original type
                OTelSdkError::InternalFailure(modified)
            })
        })
    }

    fn shutdown(&mut self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.inner.set_resource(resource)
    }
}

/// Wrapper that modifies metrics export errors to include exporter name
pub(crate) struct NamedMetricsExporter<E> {
    name: &'static str,
    inner: E,
}

impl<E> NamedMetricsExporter<E> {
    pub(crate) fn new(inner: E, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<E: PushMetricExporter> Debug for NamedMetricsExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedMetricsExporter")
            .field("name", &self.name)
            .finish()
    }
}

fn prefix_metrics_error(name: &'static str, err: OTelSdkError) -> OTelSdkError {
    let modified = format!("[{} metrics] {}", name, err);
    // Recreate as an internal failure to allow us to write a tagged message. This has
    // the unfortunate side effect of removing the original type
    OTelSdkError::InternalFailure(modified)
}

#[async_trait]
impl<E: PushMetricExporter> PushMetricExporter for NamedMetricsExporter<E> {
    fn export(&self, metrics: &ResourceMetrics) -> impl Future<Output = OTelSdkResult> + Send {
        self.inner
            .export(metrics)
            .map_err(|err| prefix_metrics_error(self.name, err))
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner
            .force_flush()
            .map_err(|err| prefix_metrics_error(self.name, err))
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner
            .shutdown()
            .map_err(|err| prefix_metrics_error(self.name, err))
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::time::Duration;

    use dashmap::DashMap;
    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;
    use tracing_core::Level;

    use crate::metrics::FutureMetricsExt;

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
    async fn test_cardinality_overflow() {
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

    // Mock span exporter to test failures
    #[derive(Debug)]
    struct FailingSpanExporter;

    impl SpanExporter for FailingSpanExporter {
        fn export(
            &self,
            _batch: Vec<SpanData>,
        ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
            Box::pin(async { Err(OTelSdkError::InternalFailure("connection failed".into())) })
        }

        fn shutdown(&mut self) -> OTelSdkResult {
            Ok(())
        }

        fn set_resource(&mut self, _resource: &opentelemetry_sdk::Resource) {}
    }

    #[tokio::test]
    async fn test_named_span_exporter_adds_prefix() {
        let inner = FailingSpanExporter;
        let named = super::NamedSpanExporter::new(inner, "test-exporter");

        let result = named.export(vec![]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("[test-exporter traces]"));
        assert!(err_msg.contains("connection failed"));
    }

    // Mock metrics exporter to test failures
    #[derive(Debug)]
    struct FailingMetricsExporter {
        error_type: &'static str,
    }

    impl PushMetricExporter for FailingMetricsExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            match self.error_type {
                "other" => Err(OTelSdkError::InternalFailure("export failed".to_string())),
                "config" => Err(OTelSdkError::InternalFailure("invalid config".to_string())),
                _ => Ok(()),
            }
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown(&self) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    #[tokio::test]
    async fn test_named_metrics_exporter_adds_prefix() {
        let inner = FailingMetricsExporter {
            error_type: "other",
        };
        let named = super::NamedMetricsExporter::new(inner, "test-exporter");

        let result = named.export(&ResourceMetrics::default()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            OTelSdkError::InternalFailure(msg) => {
                assert!(msg.contains("[test-exporter metrics]"));
                assert!(msg.contains("export failed"));
            }
            _ => panic!("Expected MetricsError::Other, got: {:?}", err),
        }
    }

    #[test]
    fn test_prefix_metrics_error() {
        let err = OTelSdkError::InternalFailure("bad config".to_string());
        let prefixed = super::prefix_metrics_error("test-exporter", err);

        // OTelSdkError::InternalFailure to_string() automatically prepends "Operation failed:".
        match prefixed {
            OTelSdkError::InternalFailure(msg) => {
                assert_eq!(msg, "[test-exporter metrics] Operation failed: bad config");
            }
            _ => panic!("Expected Config variant"),
        }
    }
}
