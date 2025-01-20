use std::num::NonZeroU64;
use std::time::Duration;

/// A rate of requests per time period.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Tps {
    capacity: u64,
    interval: Duration,
}

impl Tps {
    /// Create a new rate.
    ///
    /// # Panics
    ///
    /// This function panics if `capacity` or `interval` is 0.
    pub(crate) fn new(capacity: NonZeroU64, interval: Duration) -> Self {
        // TODO: reconsider panic
        assert!(interval > Duration::default());

        Tps {
            capacity: capacity.into(),
            interval,
        }
    }

    pub(crate) fn capacity(&self) -> u64 {
        self.capacity
    }

    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }
}
