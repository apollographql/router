//! Named metric exporter wrapper that prefixes error messages with exporter name.
//!
//! This wrapper helps identify which exporter produced an error when multiple
//! exporters are configured.

use std::fmt::Debug;
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

/// Wrapper that modifies metric export errors to include exporter name.
pub(crate) struct NamedMetricExporter<T> {
    name: &'static str,
    inner: T,
}

impl<T> NamedMetricExporter<T> {
    pub(crate) fn new(inner: T, name: &'static str) -> Self {
        Self { name, inner }
    }
}

impl<T: Debug> Debug for NamedMetricExporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedMetricExporter")
            .field("name", &self.name)
            .field("inner", &self.inner)
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

impl<T: PushMetricExporter> PushMetricExporter for NamedMetricExporter<T> {
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
    use std::time::Duration;

    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

    use super::*;

    #[derive(Debug)]
    struct FailingMetricExporter;

    impl PushMetricExporter for FailingMetricExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            Err(OTelSdkError::InternalFailure("export failed".to_string()))
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

    #[tokio::test]
    async fn test_named_metric_exporter_adds_prefix() {
        let inner = FailingMetricExporter;
        let named = NamedMetricExporter::new(inner, "test-exporter");

        let result = named.export(&ResourceMetrics::default()).await;

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
        let prefixed = prefix_otel_error("test-exporter", err);

        match prefixed {
            OTelSdkError::InternalFailure(msg) => {
                assert_eq!(msg, "[test-exporter metrics] bad config");
            }
            _ => panic!("Expected InternalFailure variant"),
        }
    }
}
