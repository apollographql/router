use std::num::NonZeroU64;
use std::time::Duration;

/// A rate of requests per time period.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Rate {
    num: u64,
    per: Duration,
}

impl Rate {
    /// Create a new rate.
    ///
    /// # Panics
    ///
    /// This function panics if `num` or `per` is 0.
    pub(crate) fn new(num: NonZeroU64, per: Duration) -> Self {
        assert!(per > Duration::default());

        Rate {
            num: num.into(),
            per,
        }
    }

    pub(crate) fn num(&self) -> u64 {
        self.num
    }

    pub(crate) fn per(&self) -> Duration {
        self.per
    }
}
