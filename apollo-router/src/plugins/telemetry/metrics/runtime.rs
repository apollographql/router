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
    /// everything else scheduled on the same worker thread. Regression test for the fix
    /// in this PR: pin the runtime to a single worker thread, spawn a
    /// synchronously-blocking future through `Runtime::spawn`, and confirm a normal
    /// lightweight task on the same runtime still makes progress concurrently instead
    /// of waiting for the blocking one to finish.
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
}
