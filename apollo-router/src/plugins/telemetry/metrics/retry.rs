//! Retry wrapper for push metric exporters.
//!
//! Wraps a `PushMetricExporter` and retries failed exports a configurable number
//! of times with exponential backoff. Only surfaces the error after all attempts
//! are exhausted, keeping transient failures out of the logs.

use std::fmt::Debug;
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

const DEFAULT_MAX_RETRIES: usize = 3;
const BASE_BACKOFF: Duration = Duration::from_millis(100);

pub(crate) struct RetryMetricExporter<T> {
    inner: T,
    max_retries: usize,
}

impl<T> RetryMetricExporter<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

impl<T: Debug> Debug for RetryMetricExporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryMetricExporter")
            .field("max_retries", &self.max_retries)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T: PushMetricExporter> PushMetricExporter for RetryMetricExporter<T> {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let mut last_err = None;
        for attempt in 0..self.max_retries {
            match self.inner.export(metrics).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::debug!(
                        attempt = attempt + 1,
                        max_retries = self.max_retries,
                        error = %err,
                        "metric export attempt failed, will retry"
                    );
                    last_err = Some(err);
                    if attempt + 1 < self.max_retries {
                        tokio::time::sleep(BASE_BACKOFF * 2u32.pow(attempt as u32)).await;
                    }
                }
            }
        }
        Err(last_err.expect("max_retries must be >= 1"))
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::Temporality;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

    use super::*;

    #[derive(Debug)]
    struct CountingExporter {
        call_count: AtomicUsize,
        fail_until: usize,
    }

    impl CountingExporter {
        fn new(fail_until: usize) -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                fail_until,
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl PushMetricExporter for CountingExporter {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= self.fail_until {
                Err(OTelSdkError::InternalFailure("transient".into()))
            } else {
                Ok(())
            }
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Delta
        }
    }

    #[tokio::test]
    async fn succeeds_on_first_try() {
        let inner = CountingExporter::new(0);
        let exporter = RetryMetricExporter::new(inner);
        let result = exporter.export(&ResourceMetrics::default()).await;
        assert!(result.is_ok());
        assert_eq!(exporter.inner.calls(), 1);
    }

    #[tokio::test]
    async fn succeeds_after_transient_failures() {
        let inner = CountingExporter::new(2);
        let exporter = RetryMetricExporter::new(inner);
        let result = exporter.export(&ResourceMetrics::default()).await;
        assert!(result.is_ok());
        assert_eq!(exporter.inner.calls(), 3);
    }

    #[tokio::test]
    async fn fails_after_all_retries_exhausted() {
        let inner = CountingExporter::new(usize::MAX);
        let exporter = RetryMetricExporter::new(inner);
        let result = exporter.export(&ResourceMetrics::default()).await;
        assert!(result.is_err());
        assert_eq!(exporter.inner.calls(), DEFAULT_MAX_RETRIES);
    }
}
