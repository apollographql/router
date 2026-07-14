//! A [`Runtime`] for `opentelemetry_sdk`'s `PeriodicReader` that doesn't block a shared
//! tokio worker thread.
//!
//! `opentelemetry_sdk::runtime::Tokio::spawn` runs the given future via a plain
//! `tokio::spawn`, on the regular async worker pool. `PeriodicReader`'s own background
//! loop calls `futures_executor::block_on(self.exporter.export(rm))` on every scheduled
//! flush (not just on shutdown) - a real, synchronous block, not just an await. Doing
//! that on a shared worker thread can starve everything else scheduled on that thread
//! (including other connections' I/O readiness), especially under a slow or unreachable
//! export endpoint. The `Runtime` trait's own documentation acknowledges runtimes need
//! to keep working even while another thread blocks waiting for shutdown; a plain
//! `tokio::spawn` doesn't guarantee that on a runtime whose worker pool is otherwise busy.
//!
//! This spawns onto `tokio::task::spawn_blocking`'s dedicated blocking thread pool
//! instead, where a real blocking wait is expected and safe, then drives the future via
//! `Handle::block_on` (which still correctly integrates with the runtime's I/O reactor,
//! unlike `futures_executor::block_on`).
use std::future::Future;
use std::time::Duration;

use opentelemetry_sdk::runtime::Runtime;

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
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use super::*;

    /// `PeriodicReader`'s background loop drives its export via
    /// `futures_executor::block_on` - a real, synchronous block, not just an await.
    /// `Runtime::spawn` must run that on a thread dedicated to blocking work
    /// (`spawn_blocking`), not the shared async worker pool, otherwise it starves
    /// everything else scheduled on the same worker thread. Pins the runtime to a
    /// single worker thread, spawns a synchronously-blocking future through
    /// `Runtime::spawn`, and confirms a normal lightweight task on the same runtime
    /// still makes progress concurrently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_does_not_block_other_tasks_on_the_same_worker() {
        let lightweight_task_completed = Arc::new(AtomicBool::new(false));
        let lightweight_task_completed_clone = lightweight_task_completed.clone();

        BlockingSafeTokio.spawn(async {
            // Mirrors `PeriodicReader`'s real, synchronous block inside its background
            // loop - not just an await.
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
