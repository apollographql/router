//! Blocking-safe Tokio runtime for OpenTelemetry SDK background tasks.
//!
//! Both `PeriodicReader` (metrics) and `BatchSpanProcessor` (tracing) drive their background
//! tasks via `futures_executor::block_on` — a real synchronous block, not just an await.
//! Running that on a shared async worker thread can starve other work on the same thread;
//! under high concurrency it can even deadlock (every thread occupied by a blocked caller,
//! none left to run the background task they are waiting on).
//!
//! [`BlockingSafeTokioRuntime`] spawns onto `tokio::task::spawn_blocking`'s dedicated thread
//! pool instead, where blocking is expected and safe.
use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

use opentelemetry_sdk::runtime::Runtime;
use opentelemetry_sdk::runtime::RuntimeChannel;
use opentelemetry_sdk::runtime::TrySend;
use opentelemetry_sdk::runtime::TrySendError;

/// Blocking-safe Tokio runtime for OpenTelemetry SDK background tasks.
///
/// Both `PeriodicReader` (metrics) and `BatchSpanProcessor` (tracing) drive their background
/// tasks via `futures_executor::block_on` — a real synchronous block, not just an await.
/// This runtime spawns those tasks onto `tokio::task::spawn_blocking`'s dedicated thread pool,
/// where blocking is expected and safe.
///
/// Use [`new_for_tracing`][Self::new_for_tracing] for `BatchSpanProcessor` (emits
/// `apollo.router.telemetry.batch_processor.errors` metrics when spans are dropped) and
/// [`new_for_metrics`][Self::new_for_metrics] for `PeriodicReader` (no channel name needed,
/// as `PeriodicReader` does not use the `RuntimeChannel` interface).
#[derive(Debug, Clone)]
pub(crate) struct BlockingSafeTokioRuntime {
    /// Exporter name used in error metrics; `None` for the metrics-reader variant.
    name: Option<&'static str>,
}

impl BlockingSafeTokioRuntime {
    /// Creates a runtime for a `BatchSpanProcessor`.
    ///
    /// `name` is attached to `apollo.router.telemetry.batch_processor.errors` metrics
    /// when spans are dropped because the channel is full or closed.
    pub(crate) fn new_for_tracing(name: &'static str) -> Self {
        Self { name: Some(name) }
    }

    /// Creates a runtime for a `PeriodicReader`.
    ///
    /// No name is required: `PeriodicReader` only calls [`Runtime::spawn`] and
    /// [`Runtime::delay`] — it never calls [`RuntimeChannel::batch_message_channel`].
    pub(crate) fn new_for_metrics() -> Self {
        Self { name: None }
    }
}

impl Runtime for BlockingSafeTokioRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            handle.block_on(future);
        });
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send + 'static {
        tokio::time::sleep(duration)
    }
}

impl RuntimeChannel for BlockingSafeTokioRuntime {
    type Receiver<T: Debug + Send> = tokio_stream::wrappers::ReceiverStream<T>;
    type Sender<T: Debug + Send> = BatchSender<T>;

    fn batch_message_channel<T: Debug + Send>(
        &self,
        capacity: usize,
    ) -> (Self::Sender<T>, Self::Receiver<T>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            BatchSender::new(self.name, sender),
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )
    }
}

/// Sender for the batch processor channel that optionally emits metrics on send failures.
///
/// When `name` is `Some` (i.e. created via [`BlockingSafeTokioRuntime::new_for_tracing`]),
/// records `apollo.router.telemetry.batch_processor.errors` labelled with the exporter name
/// when the channel is full or closed.  Metrics are skipped for the nameless metrics-reader
/// variant.
#[derive(Debug)]
pub(crate) struct BatchSender<T> {
    name: Option<&'static str>,
    sender: tokio::sync::mpsc::Sender<T>,
}

impl<T: Send> BatchSender<T> {
    fn new(name: Option<&'static str>, sender: tokio::sync::mpsc::Sender<T>) -> Self {
        Self { name, sender }
    }
}

impl<T: Send> TrySend for BatchSender<T> {
    type Message = T;

    fn try_send(&self, item: Self::Message) -> Result<(), TrySendError> {
        self.sender.try_send(item).map_err(|err| {
            let error = match &err {
                tokio::sync::mpsc::error::TrySendError::Full(_) => "channel full",
                tokio::sync::mpsc::error::TrySendError::Closed(_) => "channel closed",
            };

            if let Some(name) = self.name {
                u64_counter!(
                    "apollo.router.telemetry.batch_processor.errors",
                    "Errors when sending to a batch processor",
                    1,
                    "name" = name,
                    "error" = error
                );
                TrySendError::Other(
                    format!(
                        "cannot send message to batch processor '{name}' as the channel is {error}"
                    )
                    .into(),
                )
            } else {
                TrySendError::Other(format!("batch processor channel {error}").into())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opentelemetry_sdk::runtime::Tokio;

    use super::*;
    use crate::metrics::FutureMetricsExt;

    // ── PeriodicReader tests (Runtime only) ─────────────────────────────────

    /// Use a single worker thread to prove that `new_for_metrics` doesn't prevent other
    /// work from going through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_does_not_block_other_tasks_on_the_same_worker() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        BlockingSafeTokioRuntime::new_for_metrics().spawn(async {
            // Mirrors `PeriodicReader`'s real, synchronous block inside its background
            // loop - not just an await.
            std::thread::sleep(Duration::from_millis(300));
        });

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = tx.send(());
        });

        tokio::time::timeout(Duration::from_millis(100), rx)
            .await
            .expect(
                "lightweight task did not complete within 100 ms: \
                 the blocking spawn may have starved the worker thread",
            )
            .expect("oneshot sender dropped before sending");
    }

    /// Shared helper: spawns `concurrent_readers` `PeriodicReader`s all calling
    /// `force_flush` concurrently on a runtime capped to `worker_threads` threads, then
    /// checks whether they all complete within `bound`.
    fn metrics_force_flush_completes_within<RT>(
        runtime: RT,
        worker_threads: usize,
        concurrent_readers: usize,
        bound: Duration,
    ) -> bool
    where
        RT: Runtime + Clone,
    {
        use opentelemetry_sdk::metrics::InMemoryMetricExporter;
        use opentelemetry_sdk::metrics::SdkMeterProvider;
        use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
        use opentelemetry_sdk::metrics::reader::MetricReader;

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(async move {
                // Keep every provider alive for the duration - dropping one would tear
                // down its reader early, rather than leaving it competing for threads.
                let mut providers = Vec::with_capacity(concurrent_readers);
                let mut flushes = Vec::with_capacity(concurrent_readers);
                for _ in 0..concurrent_readers {
                    let exporter = InMemoryMetricExporter::default();
                    let reader = PeriodicReader::builder(exporter, runtime.clone())
                        .with_interval(Duration::from_secs(3600))
                        .build();
                    providers.push(
                        SdkMeterProvider::builder()
                            .with_reader(reader.clone())
                            .build(),
                    );
                    flushes.push(tokio::spawn(async move { reader.force_flush() }));
                }
                for flush in flushes {
                    let _ = flush.await;
                }
            });
            let _ = done_tx.send(());
        });

        done_rx.recv_timeout(bound).is_ok()
    }

    #[test]
    fn regular_tokio_runtime_deadlocks_metrics_force_flush_on_a_single_worker_thread() {
        assert!(
            !metrics_force_flush_completes_within(Tokio, 1, 1, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             on a single-worker-thread runtime - if this fails, either the upstream SDK \
             changed its blocking behavior, or this test is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_runtime_does_not_deadlock_metrics_force_flush_on_a_single_worker_thread()
    {
        assert!(
            metrics_force_flush_completes_within(
                BlockingSafeTokioRuntime::new_for_metrics(),
                1,
                1,
                Duration::from_secs(2)
            ),
            "BlockingSafeTokioRuntime should not deadlock PeriodicReader::force_flush \
             on a single-worker-thread runtime"
        );
    }

    #[test]
    fn regular_tokio_runtime_deadlocks_metrics_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            !metrics_force_flush_completes_within(Tokio, 4, 8, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             when 8 readers concurrently flush on a 4-worker-thread runtime - if this fails, \
             either the upstream SDK changed, or this test itself is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_runtime_does_not_deadlock_metrics_force_flush_when_demand_exceeds_a_larger_pool()
     {
        assert!(
            metrics_force_flush_completes_within(
                BlockingSafeTokioRuntime::new_for_metrics(),
                4,
                8,
                Duration::from_secs(2)
            ),
            "BlockingSafeTokioRuntime should not deadlock PeriodicReader::force_flush \
             even when 8 readers concurrently flush on a 4-worker-thread runtime"
        );
    }

    // ── BatchSpanProcessor tests (RuntimeChannel) ────────────────────────────

    /// Shared helper: spawns `concurrent_processors` `BatchSpanProcessor`s all calling
    /// `force_flush` concurrently on a runtime capped to `worker_threads` threads, then
    /// checks whether they all complete within `bound`.
    fn tracing_force_flush_completes_within<R>(
        runtime: R,
        worker_threads: usize,
        concurrent_processors: usize,
        bound: Duration,
    ) -> bool
    where
        R: RuntimeChannel + Clone,
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
            let _ = done_tx.send(());
        });

        done_rx.recv_timeout(bound).is_ok()
    }

    #[test]
    fn regular_tokio_runtime_deadlocks_tracing_force_flush_on_a_single_worker_thread() {
        assert!(
            !tracing_force_flush_completes_within(Tokio, 1, 1, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             on a single-worker-thread runtime - if this fails, either the upstream SDK \
             changed its blocking behavior, or this test is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_runtime_does_not_deadlock_tracing_force_flush_on_a_single_worker_thread()
    {
        assert!(
            tracing_force_flush_completes_within(
                BlockingSafeTokioRuntime::new_for_tracing("test"),
                1,
                1,
                Duration::from_secs(2)
            ),
            "BlockingSafeTokioRuntime should not deadlock BatchSpanProcessor::force_flush \
             on a single-worker-thread runtime"
        );
    }

    #[test]
    fn regular_tokio_runtime_deadlocks_tracing_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            !tracing_force_flush_completes_within(Tokio, 4, 8, Duration::from_secs(2)),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             when 8 processors concurrently flush on a 4-worker-thread runtime - if this \
             fails, either the upstream SDK changed, or this test itself is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_runtime_does_not_deadlock_tracing_force_flush_when_demand_exceeds_a_larger_pool()
     {
        assert!(
            tracing_force_flush_completes_within(
                BlockingSafeTokioRuntime::new_for_tracing("test"),
                4,
                8,
                Duration::from_secs(2)
            ),
            "BlockingSafeTokioRuntime should not deadlock BatchSpanProcessor::force_flush \
             even when 8 processors concurrently flush on a 4-worker-thread runtime"
        );
    }

    // ── BatchSender channel error metrics ────────────────────────────────────

    #[tokio::test]
    async fn batch_sender_channel_full_emits_metric() {
        async {
            let runtime = BlockingSafeTokioRuntime::new_for_tracing("test_processor");
            let (sender, _receiver) = runtime.batch_message_channel::<&str>(1);

            sender.try_send("first").expect("should send first message");
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
    async fn batch_sender_channel_closed_emits_metric() {
        async {
            let runtime = BlockingSafeTokioRuntime::new_for_tracing("test_processor");
            let (sender, receiver) = runtime.batch_message_channel::<&str>(1);

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
    async fn batch_sender_successful_send_emits_no_metric() {
        async {
            let runtime = BlockingSafeTokioRuntime::new_for_tracing("test_processor");
            let (sender, _receiver) = runtime.batch_message_channel::<&str>(1);

            assert!(sender.try_send("message").is_ok());

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

    #[tokio::test]
    async fn batch_sender_for_metrics_variant_emits_no_metric_on_failure() {
        async {
            let runtime = BlockingSafeTokioRuntime::new_for_metrics();
            let (sender, receiver) = runtime.batch_message_channel::<&str>(1);

            drop(receiver);
            let result = sender.try_send("message");
            assert!(result.is_err());

            // No metrics for the nameless metrics-reader variant.
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
}
