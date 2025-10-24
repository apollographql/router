use std::fmt::Debug;
use std::time::Duration;

use async_trait::async_trait;
use futures::TryFutureExt;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;

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
    fn export(&self, batch: Vec<SpanData>) ->  impl Future<Output = OTelSdkResult> + Send {
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
        let modified = format!("[{} traces] {}", name, err);
        // Recreate as an internal failure to allow us to write a tagged message. This has
        // the unfortunate side effect of removing the original type
        OTelSdkError::InternalFailure(modified)
}

#[async_trait]
impl<E: PushMetricExporter> PushMetricExporter for NamedMetricsExporter<E> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl Future<Output = OTelSdkResult> + Send {
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
    use std::ops::DerefMut;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;
    use parking_lot::Mutex;
    use tracing_core::field::Visit;
    use tracing_core::Event;
    use tracing_core::Field;
    use tracing_core::Subscriber;
    use tracing_futures::WithSubscriber;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_handle_error_throttling() {
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
            ::tracing::error!("other error");
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;

        test_layer.assert_log_entry_count("other error", 1);
        test_layer.assert_log_entry_count("trace error", 1);

        // Sleep a bit and then log again, it should get logged
        tokio::time::sleep(Duration::from_millis(200)).await;
        async {
            ::tracing::error!("other error");
        }
        .with_subscriber(tracing_subscriber::registry().with(test_layer.clone()))
        .await;
        test_layer.assert_log_entry_count("other error", 2);
    }

    #[tokio::test]
    async fn test_cardinality_overflow() {
        async {
            let msg = "Warning: Maximum data points for metric stream exceeded. Entry added to overflow. Subsequent overflows to same metric until next collect will not be logged.";
            ::tracing::warn!("{}", msg);

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

    impl PushMetricExporter for FailingMetricsExporter {
        async fn export(
            &self,
            _metrics: &ResourceMetrics,
        ) -> OTelSdkResult {
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

        let result = named.export(&mut ResourceMetrics::default()).await;

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

        match prefixed {
            OTelSdkError::InternalFailure(msg) => {
                assert_eq!(msg, "[test-exporter metrics] bad config");
            }
            _ => panic!("Expected Config variant"),
        }
    }
}
