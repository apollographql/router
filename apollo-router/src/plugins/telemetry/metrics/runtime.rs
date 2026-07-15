//! A [`Runtime`] for `opentelemetry_sdk`'s `PeriodicReader` that doesn't block a shared
//! tokio worker thread.
use std::future::Future;
use std::time::Duration;

use opentelemetry_sdk::runtime::Runtime;

/// A [`Runtime`] for `opentelemetry_sdk`'s `PeriodicReader` that doesn't block a shared
/// tokio worker thread.
///
/// `opentelemetry_sdk::runtime::Tokio::spawn` runs the given future via a plain
/// `tokio::spawn`, on the regular async worker pool. `PeriodicReader` calls
/// `futures_executor::block_on` inside the spawned task, and it can take a long
/// time because it usually does network I/O.
///
/// This version spawns onto Tokio's dedicated blocking thread pool instead.
#[derive(Debug, Clone, Default)]
pub(crate) struct BlockingSafeTokio;

impl Runtime for BlockingSafeTokio {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a single worker thread to prove that `BlockingSafeTokio` doesn't prevent other
    /// work from going through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_does_not_block_other_tasks_on_the_same_worker() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        BlockingSafeTokio.spawn(async {
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

    /// Simulates `concurrent_readers` real `PeriodicReader`s (each with its own
    /// registered `SdkMeterProvider`) all calling the real, synchronous `force_flush`
    /// at once, on a runtime with only `worker_threads` threads - e.g. a reload
    /// tearing down every active exporter simultaneously, on a runtime sized to a
    /// CPU-constrained container (`worker_threads` defaults to
    /// `available_parallelism()`, which reflects a cgroup CPU quota, not the host's
    /// full core count).
    ///
    /// `force_flush` blocks its caller waiting for a reply from the reader's own
    /// background worker task. With more concurrent callers than worker threads, every
    /// thread ends up occupied by a blocked caller before any worker task gets a
    /// chance to run - a deadlock with the plain `opentelemetry_sdk::runtime::Tokio`,
    /// and not with `BlockingSafeTokio`, whose worker task runs on a separate,
    /// dedicated `spawn_blocking` thread instead.
    fn force_flush_completes_within<RT>(
        runtime: RT,
        worker_threads: usize,
        concurrent_readers: usize,
        bound: Duration,
    ) -> bool
    where
        RT: Runtime,
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
            !force_flush_completes_within(
                opentelemetry_sdk::runtime::Tokio,
                1,
                1,
                Duration::from_secs(2)
            ),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             on a single-worker-thread runtime - if this fails, either the upstream SDK \
             changed its blocking behavior, or this test is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_does_not_deadlock_force_flush_on_a_single_worker_thread() {
        assert!(
            force_flush_completes_within(BlockingSafeTokio, 1, 1, Duration::from_secs(2)),
            "BlockingSafeTokio should not deadlock force_flush on a single-worker-thread runtime"
        );
    }

    /// Same failure mode, but demonstrating it doesn't require a literal
    /// single-worker-thread runtime: 4 worker threads, 8 readers concurrently calling
    /// `force_flush` - more simultaneous demand than the pool has capacity for, e.g.
    /// several slow/blocking exporters all flushing at once, or one reload tearing
    /// down every active exporter simultaneously.
    #[test]
    fn regular_tokio_runtime_deadlocks_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            !force_flush_completes_within(
                opentelemetry_sdk::runtime::Tokio,
                4,
                8,
                Duration::from_secs(2)
            ),
            "expected the plain opentelemetry_sdk::runtime::Tokio to deadlock force_flush \
             when 8 readers concurrently flush on a 4-worker-thread runtime - if this fails, \
             either the upstream SDK changed, or this test itself is unreliable"
        );
    }

    #[test]
    fn blocking_safe_tokio_does_not_deadlock_force_flush_when_demand_exceeds_a_larger_pool() {
        assert!(
            force_flush_completes_within(BlockingSafeTokio, 4, 8, Duration::from_secs(2)),
            "BlockingSafeTokio should not deadlock force_flush even when 8 readers concurrently \
             flush on a 4-worker-thread runtime"
        );
    }
}
