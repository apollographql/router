use std::fmt::Debug;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::BoxFuture;
use once_cell::sync::OnceCell;
use opentelemetry::metrics::MetricsError;
use opentelemetry_sdk::export::trace::ExportResult;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::export::trace::SpanExporter;
use opentelemetry_sdk::metrics::Aggregation;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::data::Temporality;
use opentelemetry_sdk::metrics::exporter::PushMetricsExporter;
use opentelemetry_sdk::metrics::reader::AggregationSelector;
use opentelemetry_sdk::metrics::reader::TemporalitySelector;

#[derive(Eq, PartialEq, Hash)]
enum ErrorType {
    Trace,
    Metric,
    Other,
}
static OTEL_ERROR_LAST_LOGGED: OnceCell<DashMap<ErrorType, Instant>> = OnceCell::new();

pub(crate) fn handle_error<T: Into<opentelemetry::global::Error>>(err: T) {
    // We have to rate limit these errors because when they happen they are very frequent.
    // Use a dashmap to store the message type with the last time it was logged.
    handle_error_with_map(err, OTEL_ERROR_LAST_LOGGED.get_or_init(DashMap::new));
}

// Allow for map injection to avoid using global map in tests
fn handle_error_with_map<T: Into<opentelemetry::global::Error>>(
    err: T,
    last_logged_map: &DashMap<ErrorType, Instant>,
) {
    let err = err.into();

    // We don't want the dashmap to get big, so we key the error messages by type.
    let error_type = match err {
        opentelemetry::global::Error::Trace(_) => ErrorType::Trace,
        opentelemetry::global::Error::Metric(_) => ErrorType::Metric,
        _ => ErrorType::Other,
    };
    #[cfg(not(test))]
    let threshold = Duration::from_secs(10);
    #[cfg(test)]
    let threshold = Duration::from_millis(100);

    if let opentelemetry::global::Error::Metric(err) = &err {
        // For now we have to suppress Metrics error: reader is shut down or not registered
        // https://github.com/open-telemetry/opentelemetry-rust/issues/1244

        if err.to_string() == "Metrics error: reader is shut down or not registered" {
            return;
        }

        // Keep track of the number of cardinality overflow errors otel emits. This can be removed after upgrading to 0.28.0 when the cardinality limit is removed.
        // The version upgrade will also cause this log to be removed from our visibility even if we were set up custom a cardinality limit.
        // https://github.com/open-telemetry/opentelemetry-rust/pull/2528
        if err.to_string()
            == "Metrics error: Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged."
        {
            u64_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                "A count of how often a telemetry metric hit the hard cardinality limit",
                1
            );
        }
    }

    // Copy here so that we don't retain a mutable reference into the dashmap and lock the shard
    let now = Instant::now();
    let last_logged = *last_logged_map
        .entry(error_type)
        .and_modify(|last_logged| {
            if last_logged.elapsed() > threshold {
                *last_logged = now;
            }
        })
        .or_insert_with(|| now);

    if last_logged == now {
        // These events are logged with explicitly no parent. This allows them to be detached from traces.
        match err {
            opentelemetry::global::Error::Trace(err) => {
                ::tracing::error!("OpenTelemetry trace error occurred: {}", err)
            }
            opentelemetry::global::Error::Metric(err) => {
                if let MetricsError::Other(msg) = &err {
                    if msg.contains("Warning") {
                        ::tracing::warn!(parent: None, "OpenTelemetry metric warning occurred: {}", msg);
                        return;
                    }

                    // TODO: We should be able to remove this after upgrading to 0.26.0, which addresses the double-shutdown
                    // called out in https://github.com/open-telemetry/opentelemetry-rust/issues/1661
                    if msg == "metrics provider already shut down" {
                        return;
                    }
                }
                ::tracing::error!(parent: None, "OpenTelemetry metric error occurred: {}", err);
            }
            opentelemetry::global::Error::Other(err) => {
                ::tracing::error!(parent: None, "OpenTelemetry error occurred: {}", err)
            }
            other => {
                ::tracing::error!(parent: None, "OpenTelemetry error occurred: {:?}", other)
            }
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
    fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let name = self.name;
        let fut = self.inner.export(batch);
        Box::pin(async move {
            fut.await.map_err(|err| {
                let modified = format!("[{} traces] {}", name, err);
                opentelemetry::trace::TraceError::from(modified)
            })
        })
    }

    fn shutdown(&mut self) {
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

impl<E: PushMetricsExporter> Debug for NamedMetricsExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedMetricsExporter")
            .field("name", &self.name)
            .finish()
    }
}

impl<E: AggregationSelector> AggregationSelector for NamedMetricsExporter<E> {
    fn aggregation(&self, kind: InstrumentKind) -> Aggregation {
        self.inner.aggregation(kind)
    }
}

impl<E: TemporalitySelector> TemporalitySelector for NamedMetricsExporter<E> {
    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.inner.temporality(kind)
    }
}

fn prefix_metrics_error(name: &'static str, err: MetricsError) -> MetricsError {
    match err {
        MetricsError::Other(msg) => MetricsError::Other(format!("[{} metrics] {}", name, msg)),
        MetricsError::Config(msg) => MetricsError::Config(format!("[{} metrics] {}", name, msg)),
        MetricsError::ExportErr(inner) => {
            MetricsError::Other(format!("[{} metrics] {}", name, inner))
        }
        // Don't modify instrument configuration errors - not related to export
        MetricsError::InvalidInstrumentConfiguration(msg) => {
            MetricsError::InvalidInstrumentConfiguration(msg)
        }
        _ => MetricsError::Other(format!("[{} metrics] {}", name, err)),
    }
}

#[async_trait]
impl<E: PushMetricsExporter> PushMetricsExporter for NamedMetricsExporter<E> {
    async fn export(&self, metrics: &mut ResourceMetrics) -> opentelemetry::metrics::Result<()> {
        self.inner
            .export(metrics)
            .await
            .map_err(|err| prefix_metrics_error(self.name, err))
    }

    async fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
        self.inner
            .force_flush()
            .await
            .map_err(|err| prefix_metrics_error(self.name, err))
    }

    fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
        self.inner
            .shutdown()
            .map_err(|err| prefix_metrics_error(self.name, err))
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::ops::DerefMut;
    use std::sync::Arc;
    use std::time::Duration;

    use dashmap::DashMap;
    use futures::future::BoxFuture;
    use opentelemetry::metrics::MetricsError;
    use opentelemetry_sdk::export::trace::SpanData;
    use opentelemetry_sdk::export::trace::SpanExporter;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricsExporter;
    use parking_lot::Mutex;
    use tracing_core::Event;
    use tracing_core::Field;
    use tracing_core::Subscriber;
    use tracing_core::field::Visit;
    use tracing_futures::WithSubscriber;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::error_handler::handle_error_with_map;

    #[tokio::test]
    async fn test_handle_error_throttling() {
        let error_map = DashMap::new();
        // Set up a fake subscriber so we can check log events. If this is useful then maybe it can be factored out into something reusable
        #[derive(Default)]
        struct TestVisitor {
            log_entries: Vec<String>,
        }

        #[derive(Default, Clone)]
        struct TestLayer {
            visitor: Arc<Mutex<TestVisitor>>,
        }
        impl TestLayer {
            fn assert_log_entry_count(&self, message: &str, expected: usize) {
                let log_entries = self.visitor.lock().log_entries.clone();
                let actual = log_entries.iter().filter(|e| e.contains(message)).count();
                assert_eq!(actual, expected);
            }
        }
        impl Visit for TestVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                self.log_entries
                    .push(format!("{}={:?}", field.name(), value));
            }
        }

        impl<S> Layer<S> for TestLayer
        where
            S: Subscriber,
            Self: 'static,
        {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                event.record(self.visitor.lock().deref_mut())
            }
        }

        let test_layer = TestLayer::default();

        async {
            // Log twice rapidly, they should get deduped
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
            handle_error_with_map(
                opentelemetry::global::Error::Trace("trace error".to_string().into()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            handle_error_with_map(
                opentelemetry::global::Error::Other("other error".to_string()),
                &error_map,
            );
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }

    #[tokio::test]
    async fn test_cardinality_overflow() {
        async {
            let error_map = DashMap::new();
            let msg = "Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged.";
            handle_error_with_map(
                opentelemetry::global::Error::Metric(opentelemetry::metrics::MetricsError::Other(msg.to_string())),
                &error_map,
            );

            assert_counter!(
                "apollo.router.telemetry.metrics.cardinality_overflow",
                1
            );
        }
        .with_metrics()
        .await;
    }

    // Mock span exporter to test failures
    #[derive(Debug)]
    struct FailingSpanExporter;

    impl SpanExporter for FailingSpanExporter {
        fn export(
            &mut self,
            _batch: Vec<SpanData>,
        ) -> BoxFuture<'static, opentelemetry_sdk::export::trace::ExportResult> {
            Box::pin(async { Err(opentelemetry::trace::TraceError::from("connection failed")) })
        }

        fn shutdown(&mut self) {}

        fn set_resource(&mut self, _resource: &opentelemetry_sdk::Resource) {}
    }

    #[tokio::test]
    async fn test_named_span_exporter_adds_prefix() {
        let inner = FailingSpanExporter;
        let mut named = super::NamedSpanExporter::new(inner, "test-exporter");

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

    #[async_trait::async_trait]
    impl PushMetricsExporter for FailingMetricsExporter {
        async fn export(
            &self,
            _metrics: &mut ResourceMetrics,
        ) -> opentelemetry::metrics::Result<()> {
            match self.error_type {
                "other" => Err(MetricsError::Other("export failed".to_string())),
                "config" => Err(MetricsError::Config("invalid config".to_string())),
                _ => Ok(()),
            }
        }

        async fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
            Ok(())
        }

        fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
            Ok(())
        }
    }

    impl opentelemetry_sdk::metrics::reader::AggregationSelector for FailingMetricsExporter {
        fn aggregation(
            &self,
            _kind: opentelemetry_sdk::metrics::InstrumentKind,
        ) -> opentelemetry_sdk::metrics::Aggregation {
            opentelemetry_sdk::metrics::Aggregation::Default
        }
    }

    impl opentelemetry_sdk::metrics::reader::TemporalitySelector for FailingMetricsExporter {
        fn temporality(
            &self,
            _kind: opentelemetry_sdk::metrics::InstrumentKind,
        ) -> opentelemetry_sdk::metrics::data::Temporality {
            opentelemetry_sdk::metrics::data::Temporality::Cumulative
        }
    }

    fn empty_resource_metrics() -> ResourceMetrics {
        use opentelemetry_sdk::Resource;
        ResourceMetrics {
            resource: Resource::empty(),
            scope_metrics: vec![],
        }
    }

    #[tokio::test]
    async fn test_named_metrics_exporter_adds_prefix() {
        let inner = FailingMetricsExporter {
            error_type: "other",
        };
        let named = super::NamedMetricsExporter::new(inner, "test-exporter");

        let result = named.export(&mut empty_resource_metrics()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            MetricsError::Other(msg) => {
                assert!(msg.contains("[test-exporter metrics]"));
                assert!(msg.contains("export failed"));
            }
            _ => panic!("Expected MetricsError::Other, got: {:?}", err),
        }
    }

    #[test]
    fn test_prefix_metrics_error() {
        let err = MetricsError::Config("bad config".to_string());
        let prefixed = super::prefix_metrics_error("test-exporter", err);

        match prefixed {
            MetricsError::Config(msg) => {
                assert_eq!(msg, "[test-exporter metrics] bad config");
            }
            _ => panic!("Expected Config variant"),
        }
    }
}
