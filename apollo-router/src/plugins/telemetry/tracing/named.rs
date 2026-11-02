//! Named wrappers for OpenTelemetry components.
//!
//! This module provides wrappers that add exporter name context to errors:
//! - [`NamedSpanExporter`]: Prefixes export error messages with the exporter name.
//!
//! For the blocking-safe Tokio runtime (used for both tracing and metrics background tasks),
//! see [`crate::plugins::telemetry::metrics::runtime::BlockingSafeTokioRuntime`].

use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;

/// Wrapper that modifies trace export errors to include the exporter name.
pub(crate) struct NamedSpanExporter<E> {
    name: &'static str,
    inner: E,
}

impl<E> NamedSpanExporter<E> {
    pub(crate) fn new(inner: E, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<E: SpanExporter> std::fmt::Debug for NamedSpanExporter<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedSpanExporter")
            .field("name", &self.name)
            .finish()
    }
}

impl<E: SpanExporter> SpanExporter for NamedSpanExporter<E> {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(batch);
        async move {
            fut.await
                .map_err(|err| OTelSdkError::InternalFailure(format!("[{name} traces] {err}")))
        }
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.inner.set_resource(resource)
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;

    use super::*;

    #[derive(Debug)]
    struct FailingSpanExporter;

    impl SpanExporter for FailingSpanExporter {
        async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
            Err(OTelSdkError::InternalFailure(
                "connection failed".to_string(),
            ))
        }

        fn shutdown(&self) -> OTelSdkResult {
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn set_resource(&mut self, _resource: &opentelemetry_sdk::Resource) {}
    }

    #[tokio::test]
    async fn test_named_span_exporter_adds_prefix() {
        let inner = FailingSpanExporter;
        let named = NamedSpanExporter::new(inner, "test-exporter");

        let result = named.export(vec![]).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("[test-exporter traces]"));
        assert!(err_msg.contains("connection failed"));
    }
}
