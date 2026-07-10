//! Named wrappers for OpenTelemetry components.
//!
//! This module provides wrappers that add exporter name context to errors and metrics:
//! - `NamedSpanExporter`: Prefixes export error messages with exporter name
//! - `NamedTokioRuntime`: Emits metrics when batch processor channel operations fail

use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::runtime::Runtime;
use opentelemetry_sdk::runtime::RuntimeChannel;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::runtime::TrySend;
use opentelemetry_sdk::runtime::TrySendError;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;

/// Wrapper that modifies trace export errors to include exporter name.
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
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let name = self.name;
        let fut = self.inner.export(batch);
        async move {
            fut.await
                .map_err(|err| OTelSdkError::InternalFailure(format!("[{} traces] {}", name, err)))
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

/// Wraps the Tokio runtime to emit metrics when batch processor channel operations fail.
///
/// This enables the `apollo.router.telemetry.batch_processor.errors` metric to be
/// emitted with the exporter name when spans are dropped due to a full or closed channel.
#[derive(Debug, Clone)]
pub(crate) struct NamedTokioRuntime {
    name: &'static str,
}

impl NamedTokioRuntime {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name }
    }
}

impl Runtime for NamedTokioRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Not `Tokio.spawn(future)` (a plain `tokio::spawn`, on the shared async worker
        // pool): `BatchSpanProcessor`'s background task services `force_flush`/`shutdown`
        // requests via `futures_executor::block_on` on a oneshot channel from *its own*
        // task, and separately, callers of `force_flush`/`shutdown` themselves
        // synchronously block waiting on that same channel. Spawning this background
        // task onto a shared worker thread risks it competing with, or being starved by,
        // other work on that same thread - including the blocking caller itself if they
        // happen to land on the same worker. `spawn_blocking` runs it on a thread
        // dedicated to blocking work instead, then drives it with `Handle::block_on`,
        // which still correctly integrates with the runtime's I/O reactor.
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            handle.block_on(future);
        });
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send + 'static {
        Tokio.delay(duration)
    }
}

impl RuntimeChannel for NamedTokioRuntime {
    type Receiver<T: Debug + Send> = <Tokio as RuntimeChannel>::Receiver<T>;
    type Sender<T: Debug + Send> = NamedSender<T>;

    fn batch_message_channel<T: Debug + Send>(
        &self,
        capacity: usize,
    ) -> (Self::Sender<T>, Self::Receiver<T>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            NamedSender::new(self.name, sender),
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )
    }
}

/// A channel sender that emits metrics when send operations fail.
#[derive(Debug)]
pub(crate) struct NamedSender<T> {
    name: &'static str,
    channel_full_message: String,
    channel_closed_message: String,
    sender: tokio::sync::mpsc::Sender<T>,
}

impl<T: Send> NamedSender<T> {
    fn new(name: &'static str, sender: tokio::sync::mpsc::Sender<T>) -> Self {
        Self {
            name,
            channel_full_message: format!(
                "cannot send message to batch processor '{name}' as the channel is full"
            ),
            channel_closed_message: format!(
                "cannot send message to batch processor '{name}' as the channel is closed"
            ),
            sender,
        }
    }
}

impl<T: Send> TrySend for NamedSender<T> {
    type Message = T;

    fn try_send(&self, item: Self::Message) -> Result<(), TrySendError> {
        self.sender.try_send(item).map_err(|err| {
            let error = match &err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => "channel full",
                tokio::sync::mpsc::error::TrySendError::Closed(_) => "channel closed",
            };
            u64_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                "Errors when sending to a batch processor",
                1,
                "name" = self.name,
                "error" = error
            );

            match err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    TrySendError::Other(self.channel_full_message.as_str().into())
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    TrySendError::Other(self.channel_closed_message.as_str().into())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::error::OTelSdkError;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::SpanData;
    use opentelemetry_sdk::trace::SpanExporter;

    use super::*;
    use crate::metrics::FutureMetricsExt;

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
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("[test-exporter traces]"));
        assert!(err_msg.contains("connection failed"));
    }

    #[tokio::test]
    async fn test_named_runtime_channel_full_emits_metric() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, _receiver) = runtime.batch_message_channel::<&str>(1);

            // Fill the channel
            sender.try_send("first").expect("should send first message");

            // This should fail and emit metrics
            let result = sender.try_send("second");
            assert!(result.is_err());

            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                1,
                "name" = "test_processor",
                "error" = "channel full"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_named_runtime_channel_closed_emits_metric() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, receiver) = runtime.batch_message_channel::<&str>(1);

            // Drop receiver to close channel
            drop(receiver);

            let result = sender.try_send("message");
            assert!(result.is_err());

            assert_counter!(
                "apollo.router.telemetry.batch_processor.errors",
                1,
                "name" = "test_processor",
                "error" = "channel closed"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_named_runtime_successful_send_no_metric() {
        async {
            let runtime = NamedTokioRuntime::new("test_processor");
            let (sender, _receiver) = runtime.batch_message_channel::<&str>(1);

            let result = sender.try_send("message");
            assert!(result.is_ok());

            // No metrics should be emitted for success case
            let metrics = crate::metrics::collect_metrics();
            assert!(
                metrics
                    .find("apollo.router.telemetry.batch_processor.errors")
                    .is_none()
            );
        }
        .with_metrics()
        .await;
    }

    /// `PeriodicReader`/`BatchSpanProcessor` drive their background task via
    /// `futures_executor::block_on` internally - a real, synchronous block, not just an
    /// await. `Runtime::spawn` must run that on a thread dedicated to blocking work
    /// (`spawn_blocking`), not the shared async worker pool, otherwise it starves
    /// everything else scheduled on the same worker thread. Regression test for the fix
    /// in this PR: pin the runtime to a single worker thread, spawn a
    /// synchronously-blocking future through `Runtime::spawn`, and confirm a normal
    /// lightweight task on the same runtime still makes progress concurrently instead
    /// of waiting for the blocking one to finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_does_not_block_other_tasks_on_the_same_worker() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let lightweight_task_completed = Arc::new(AtomicBool::new(false));
        let lightweight_task_completed_clone = lightweight_task_completed.clone();

        let runtime = NamedTokioRuntime::new("test_processor");
        Runtime::spawn(&runtime, async {
            // Mirrors `BatchSpanProcessor`'s real, synchronous block inside its
            // background task - not just an await.
            std::thread::sleep(Duration::from_millis(300));
        });

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            lightweight_task_completed_clone.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            lightweight_task_completed.load(Ordering::SeqCst),
            "a lightweight task on the same single-worker-thread runtime was starved by \
             a synchronously-blocking future spawned through Runtime::spawn"
        );
    }
}
