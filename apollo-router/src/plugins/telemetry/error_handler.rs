use std::fmt::Debug;
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
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
    fn export(&self, batch: Vec<SpanData>) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(batch);
        async move {
            fut.await.map_err(|err| {
                OTelSdkError::InternalFailure(format!("[{} traces] {}", name, err))
            })
        }
    }

    fn shutdown(&mut self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&mut self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.inner.set_resource(resource)
    }
}

/// Wrapper that modifies metrics export errors to include exporter name
pub(crate) struct NamedMetricExporter<E> {
    name: &'static str,
    inner: E,
}

impl<E> NamedMetricExporter<E> {
    pub(crate) fn new(inner: E, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<E: PushMetricExporter> Debug for NamedMetricExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedMetricExporter")
            .field("name", &self.name)
            .finish()
    }
}

fn prefix_otel_error(name: &'static str, err: OTelSdkError) -> OTelSdkError {
    match err {
        OTelSdkError::AlreadyShutdown => OTelSdkError::AlreadyShutdown,
        OTelSdkError::Timeout(d) => OTelSdkError::Timeout(d),
        OTelSdkError::InternalFailure(msg) => {
            OTelSdkError::InternalFailure(format!("[{} metrics] {}", name, msg))
        }
    }
}

impl<E: PushMetricExporter> PushMetricExporter for NamedMetricExporter<E> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(metrics);
        async move { fut.await.map_err(|err| prefix_otel_error(name, err)) }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner
            .force_flush()
            .map_err(|err| prefix_otel_error(self.name, err))
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner
            .shutdown_with_timeout(timeout)
            .map_err(|err| prefix_otel_error(self.name, err))
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::time::Duration;

    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;

    // Mock span exporter to test failures
    #[derive(Debug)]
    struct FailingSpanExporter;

    impl SpanExporter for FailingSpanExporter {
        fn export(
            &self,
            _batch: Vec<SpanData>,
        ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
            async { Err(OTelSdkError::InternalFailure("connection failed".to_string())) }
        }

        fn shutdown(&mut self) -> OTelSdkResult {
            Ok(())
        }

        fn force_flush(&mut self) -> OTelSdkResult {
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
    struct FailingMetricExporter;

    impl PushMetricExporter for FailingMetricExporter {
        fn export(
            &self,
            _metrics: &ResourceMetrics,
        ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
            async { Err(OTelSdkError::InternalFailure("export failed".to_string())) }
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    fn empty_resource_metrics() -> ResourceMetrics {
        use opentelemetry_sdk::Resource;
        ResourceMetrics {
            resource: Resource::builder_empty().build(),
            scope_metrics: vec![],
        }
    }

    #[tokio::test]
    async fn test_named_metric_exporter_adds_prefix() {
        let inner = FailingMetricExporter;
        let named = super::NamedMetricExporter::new(inner, "test-exporter");

        let result = named.export(&empty_resource_metrics()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            OTelSdkError::InternalFailure(msg) => {
                assert!(msg.contains("[test-exporter metrics]"));
                assert!(msg.contains("export failed"));
            }
            _ => panic!("Expected InternalFailure, got: {:?}", err),
        }
    }

    #[test]
    fn test_prefix_otel_error() {
        let err = OTelSdkError::InternalFailure("bad config".to_string());
        let prefixed = super::prefix_otel_error("test-exporter", err);

        match prefixed {
            OTelSdkError::InternalFailure(msg) => {
                assert_eq!(msg, "[test-exporter metrics] bad config");
            }
            _ => panic!("Expected InternalFailure variant"),
        }
    }
}
