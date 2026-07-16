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

use crate::plugins::telemetry::metrics::runtime::BlockingSafeTokio;

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
        // Delegate to BlockingSafeTokio to avoid duplicating the spawn_blocking logic.
        BlockingSafeTokio.spawn(future);
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send + 'static {
        BlockingSafeTokio.delay(duration)
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
    /// everything else scheduled on the same worker thread. Pins the runtime to a
    /// single worker thread, spawns a synchronously-blocking future through
    /// `Runtime::spawn`, and confirms a normal lightweight task on the same runtime
    /// still makes progress concurrently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_does_not_block_other_tasks_on_the_same_worker() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let runtime = NamedTokioRuntime::new("test_processor");
        Runtime::spawn(&runtime, async {
            // Mirrors `BatchSpanProcessor`'s real, synchronous block inside its
            // background task - not just an await.
            std::thread::sleep(Duration::from_millis(300));
        });

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = tx.send(());
        });

        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect(
                "lightweight task did not complete within 5 s: \
                 the blocking spawn may have starved the worker thread",
            )
            .expect("oneshot sender dropped before sending");
    }

    /// Simulates `concurrent_processors` real `BatchSpanProcessor`s all calling the
    /// real, synchronous `force_flush` at once, on a runtime with only
    /// `worker_threads` threads - e.g. a reload tearing down every active exporter
    /// simultaneously, on a runtime sized to a CPU-constrained container
    /// (`worker_threads` defaults to `available_parallelism()`, which reflects a
    /// cgroup CPU quota, not the host's full core count).
    ///
    /// `force_flush` blocks its caller waiting for a reply from the processor's own
    /// background worker task. With more concurrent callers than worker threads, every
    /// thread ends up occupied by a blocked caller before any worker task gets a
    /// chance to run - a deadlock with the plain `opentelemetry_sdk::runtime::Tokio`,
    /// and not with `NamedTokioRuntime`, whose worker task runs on a separate,
    /// dedicated `spawn_blocking` thread instead.
    fn force_flush_completes_within<R>(
        runtime: R,
        worker_threads: usize,
        concurrent_processors: usize,
        bound: Duration,
    ) -> bool
    where
        R: RuntimeChannel,
    {
        use opentelemetry_sdk::trace::InMemorySpanExporterBuilder;
        use opentelemetry_sdk::trace::SpanProcessor;
        use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(async move {
                let mut flushes = Vec::with_capacity(concurrent_processors);
                for _ in 0..concurrent_processors {
                    let exporter = InMemorySpanExporterBuilder::new().build();
                    let processor = BatchSpanProcessor::builder(exporter, runtime.clone()).build();
                    flushes.push(tokio::spawn(async move { processor.force_flush() }));
                }
                for flush in flushes {
                    let _ = flush.await;
                }
            });
            // Only reached if the block_on above actually returned, i.e. every
            // force_flush call above completed without deadlocking.
            let _ = done_tx.send(());
        });

        // This function has no tokio runtime of its own to protect - a plain,
        // synchronous, bounded wait is all that's needed.
        done_rx.recv_timeout(bound).is_ok()
    }

    #[test]
    fn regular_tokio_runtime_deadlocks_force_flush_on_a_single_worker_thread() {
        assert!(
            !force_flush_completes_within(Tokio, 1, 1, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             on a single-worker-thread runtime - if this fails, either the upstream SDK \
             changed its blocking behavior, or this test is unreliable"
        );
    }

    #[test]
    fn named_tokio_runtime_does_not_deadlock_force_flush_on_a_single_worker_thread() {
        assert!(
            force_flush_completes_within(
                NamedTokioRuntime::new("test_processor"),
                1,
                1,
                Duration::from_secs(2)
            ),
            "NamedTokioRuntime should not deadlock force_flush on a single-worker-thread runtime"
        );
    }

    /// Same failure mode, but demonstrating it doesn't require a literal
    /// single-worker-thread runtime: 4 worker threads, 8 processors concurrently
    /// calling `force_flush` - more simultaneous demand than the pool has capacity
    /// for, e.g. several slow/blocking exporters all flushing at once, or one reload
    /// tearing down every active exporter simultaneously.
    #[test]
    fn regular_tokio_runtime_deadlocks_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            !force_flush_completes_within(Tokio, 4, 8, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             when 8 processors concurrently flush on a 4-worker-thread runtime - if this \
             fails, either the upstream SDK changed, or this test itself is unreliable"
        );
    }

    #[test]
    fn named_tokio_runtime_does_not_deadlock_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            force_flush_completes_within(
                NamedTokioRuntime::new("test_processor"),
                4,
                8,
                Duration::from_secs(2)
            ),
            "NamedTokioRuntime should not deadlock force_flush even when 8 processors \
             concurrently flush on a 4-worker-thread runtime"
        );
    }
}
