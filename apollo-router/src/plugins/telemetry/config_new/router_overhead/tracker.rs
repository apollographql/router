use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use parking_lot::Mutex;

use super::guard::SubgraphRequestGuard;

/// Result of calculating router overhead.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OverheadResult {
    /// The calculated router overhead (total time - subgraph time)
    pub(crate) overhead: Duration,
    /// The number of active subgraph requests
    pub(crate) active_subgraph_requests: u64,
}

/// Tracks router overhead by measuring time NOT spent waiting for subgraph requests.
///
/// The tracker maintains:
/// - Total request start time
/// - Accumulated time when subgraph requests are in flight
/// - Count of currently active subgraph requests
///
/// Router overhead = total_time - accumulated_subgraph_time
#[derive(Debug, Clone)]
pub(crate) struct RouterOverheadTracker {
    /// When the request started
    request_start: Instant,

    /// Shared state protected by mutex
    inner: Arc<Mutex<TrackerInner>>,
}

#[derive(Debug)]
pub(in crate::plugins::telemetry) struct TrackerInner {
    /// Accumulated time when one or more subgraph requests were in flight
    pub(in crate::plugins::telemetry) accumulated_subgraph_time: Duration,

    /// When the most recent period of subgraph activity started
    /// (None if no subgraph requests are currently active)
    pub(in crate::plugins::telemetry) current_period_start: Option<Instant>,

    /// Count of active subgraph requests
    pub(in crate::plugins::telemetry) active_count: u64,
}

impl Default for RouterOverheadTracker {
    fn default() -> Self {
        Self {
            request_start: Instant::now(),
            inner: Arc::new(Mutex::new(TrackerInner {
                accumulated_subgraph_time: Duration::ZERO,
                current_period_start: None,
                active_count: 0,
            })),
        }
    }
}

impl RouterOverheadTracker {
    /// Creates a new tracker for a request
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Creates a guard for a subgraph request.
    /// When the guard is dropped, the subgraph request is considered complete.
    pub(crate) fn create_guard(&self) -> SubgraphRequestGuard {
        SubgraphRequestGuard::new(self.inner.clone())
    }

    /// Calculates the router overhead.
    /// This should be called at the end of the request.
    ///
    /// Returns the overhead duration and whether there are still active subgraph requests.
    /// If there are active requests, the overhead calculation includes time up to now.
    pub(crate) fn calculate_overhead(&self) -> OverheadResult {
        let total_elapsed = self.request_start.elapsed();

        let inner = self.inner.lock();
        let active_count = inner.active_count;

        // If there are still active subgraph requests, accumulate the current period
        let accumulated_time = if active_count > 0 {
            if let Some(period_start) = inner.current_period_start {
                inner.accumulated_subgraph_time + period_start.elapsed()
            } else {
                inner.accumulated_subgraph_time
            }
        } else {
            inner.accumulated_subgraph_time
        };

        // Overhead is time NOT spent waiting for subgraphs
        let overhead = total_elapsed.saturating_sub(accumulated_time);

        OverheadResult {
            overhead,
            active_subgraph_requests: active_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn test_single_subgraph_request() {
        let tracker = RouterOverheadTracker::new();

        // Simulate a subgraph request that takes 100ms
        {
            let _guard = tracker.create_guard();
            thread::sleep(Duration::from_millis(100));
        }

        // Wait a bit more for router processing
        thread::sleep(Duration::from_millis(50));

        let result = tracker.calculate_overhead();

        // Overhead should be roughly 50ms (the time not spent in subgraph)
        // On overloaded CI systems, timing can be very imprecise, so we use generous bounds
        // We're testing the logic is correct, not that thread::sleep is precise
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(
            result.overhead >= Duration::from_millis(30)
                && result.overhead <= Duration::from_millis(200),
            "overhead was {:?}",
            result.overhead
        );
    }

    #[test]
    fn test_sequential_subgraph_requests() {
        let tracker = RouterOverheadTracker::new();

        // First subgraph request
        {
            let _guard = tracker.create_guard();
            thread::sleep(Duration::from_millis(50));
        }

        // Router processing time
        thread::sleep(Duration::from_millis(20));

        // Second subgraph request
        {
            let _guard = tracker.create_guard();
            thread::sleep(Duration::from_millis(50));
        }

        let result = tracker.calculate_overhead();

        // Overhead should be roughly 20ms (the time between subgraph requests)
        // On overloaded CI systems, timing can be very imprecise, so we use generous bounds
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(
            result.overhead >= Duration::from_millis(5)
                && result.overhead <= Duration::from_millis(100),
            "overhead was {:?}",
            result.overhead
        );
    }

    #[test]
    fn test_concurrent_subgraph_requests() {
        let tracker = RouterOverheadTracker::new();

        thread::sleep(Duration::from_millis(10));

        // Create two overlapping subgraph requests
        let guard1 = tracker.create_guard();
        thread::sleep(Duration::from_millis(50));

        let guard2 = tracker.create_guard();
        thread::sleep(Duration::from_millis(50));

        drop(guard1); // First request completes
        thread::sleep(Duration::from_millis(50));

        drop(guard2); // Second request completes

        let result = tracker.calculate_overhead();

        // Total time: ~160ms
        // Subgraph time: 100ms (guard1) + 50ms (only guard2) = 150ms
        // Overhead: ~10ms initial + some processing = ~10-20ms
        // On overloaded CI systems, timing can be very imprecise, so we use generous bounds
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(
            result.overhead >= Duration::from_millis(3)
                && result.overhead <= Duration::from_millis(100),
            "overhead was {:?}",
            result.overhead
        );
    }

    #[test]
    fn test_no_subgraph_requests() {
        let tracker = RouterOverheadTracker::new();

        thread::sleep(Duration::from_millis(100));

        let result = tracker.calculate_overhead();

        // All time is overhead when there are no subgraph requests
        // On overloaded CI systems, timing can be very imprecise, so we use generous bounds
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(
            result.overhead >= Duration::from_millis(80)
                && result.overhead <= Duration::from_millis(250),
            "overhead was {:?}",
            result.overhead
        );
    }

    #[test]
    fn test_guard_drop_on_panic() {
        let tracker = Arc::new(RouterOverheadTracker::new());
        let tracker_clone = tracker.clone();

        // Simulate a panic while holding a guard
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = tracker_clone.create_guard();
            thread::sleep(Duration::from_millis(50));
            panic!("simulated error");
        }));

        assert!(result.is_err());

        // Give time for the guard to be dropped
        thread::sleep(Duration::from_millis(10));

        // Tracker should still work and calculate overhead correctly
        let result = tracker.calculate_overhead();
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(result.overhead >= Duration::ZERO);
    }

    #[test]
    fn test_thread_safety() {
        let tracker = Arc::new(RouterOverheadTracker::new());

        let mut handles = vec![];

        // Spawn multiple threads that create guards concurrently
        for _ in 0..10 {
            let tracker_clone = tracker.clone();
            let handle = thread::spawn(move || {
                let _guard = tracker_clone.create_guard();
                thread::sleep(Duration::from_millis(10));
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Tracker should still be in a valid state
        let result = tracker.calculate_overhead();
        assert_eq!(result.active_subgraph_requests, 0);
        assert!(result.overhead >= Duration::ZERO);
    }

    #[test]
    fn test_active_subgraph_requests_flag() {
        let tracker = RouterOverheadTracker::new();

        thread::sleep(Duration::from_millis(10));

        // No active requests initially
        let result = tracker.calculate_overhead();
        assert_eq!(result.active_subgraph_requests, 0);

        // Create a guard (active request)
        let _guard = tracker.create_guard();
        thread::sleep(Duration::from_millis(10));

        // Should signal active requests
        let result = tracker.calculate_overhead();
        assert_eq!(
            result.active_subgraph_requests, 1,
            "Should have active subgraph requests"
        );

        // Drop the guard
        drop(_guard);
        thread::sleep(Duration::from_millis(10));

        // Should no longer have active requests
        let result = tracker.calculate_overhead();
        assert_eq!(
            result.active_subgraph_requests, 0,
            "Should not have active subgraph requests"
        );
    }
}
