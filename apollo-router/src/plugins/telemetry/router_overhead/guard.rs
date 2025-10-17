use std::sync::Arc;

use parking_lot::Mutex;

use super::tracker::TrackerInner;

/// A guard that tracks the lifetime of a subgraph request.
///
/// When created, it increments the active subgraph request counter.
/// When dropped, it decrements the counter and updates timing information.
///
/// This ensures accurate tracking even if the subgraph request is cancelled
/// or encounters an error.
#[derive(Debug)]
pub(crate) struct SubgraphRequestGuard {
    inner: Arc<Mutex<TrackerInner>>,
}

impl SubgraphRequestGuard {
    pub(super) fn new(inner: Arc<Mutex<TrackerInner>>) -> Self {
        let mut inner_lock = inner.lock();

        // Increment the active count
        let prev_count = inner_lock
            .active_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // If this is the first active subgraph request, start timing
        if prev_count == 0 {
            inner_lock.current_period_start = Some(std::time::Instant::now());
        }

        drop(inner_lock);

        Self { inner }
    }
}

impl Drop for SubgraphRequestGuard {
    fn drop(&mut self) {
        let mut inner = self.inner.lock();

        let prev_count = inner
            .active_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

        // If this was the last active subgraph request, accumulate the time
        if prev_count == 1 {
            if let Some(period_start) = inner.current_period_start.take() {
                let elapsed = period_start.elapsed();
                inner.accumulated_subgraph_time += elapsed;
            }
        }
    }
}
