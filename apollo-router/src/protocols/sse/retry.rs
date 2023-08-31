use std::time::{Duration, Instant};

use rand::{thread_rng, Rng};

pub(crate) trait RetryStrategy {
    /// Return the next amount of time a failed request should delay before re-attempting.
    fn next_delay(&mut self, current_time: Instant) -> Duration;

    /// Modify the strategy's default base delay.
    fn change_base_delay(&mut self, base_delay: Duration);

    /// Used to indicate to the strategy that it can reset as a successful connection has been made.
    fn reset(&mut self, current_time: Instant);
}

const DEFAULT_RESET_RETRY_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) struct BackoffRetry {
    base_delay: Duration,
    max_delay: Duration,
    backoff_factor: u32,
    include_jitter: bool,

    reset_interval: Duration,
    next_delay: Duration,
    good_since: Option<Instant>,
}

impl BackoffRetry {
    pub(crate) fn new(
        base_delay: Duration,
        max_delay: Duration,
        backoff_factor: u32,
        include_jitter: bool,
    ) -> Self {
        Self {
            base_delay,
            max_delay,
            backoff_factor,
            include_jitter,
            reset_interval: DEFAULT_RESET_RETRY_INTERVAL,
            next_delay: base_delay,
            good_since: None,
        }
    }
}

impl RetryStrategy for BackoffRetry {
    fn next_delay(&mut self, current_time: Instant) -> Duration {
        let mut current_delay = self.next_delay;

        if let Some(good_since) = self.good_since {
            if current_time - good_since >= self.reset_interval {
                current_delay = self.base_delay;
            }
        }

        self.good_since = None;
        self.next_delay = std::cmp::min(self.max_delay, current_delay * self.backoff_factor);

        if self.include_jitter {
            thread_rng().gen_range(current_delay / 2..=current_delay)
        } else {
            current_delay
        }
    }

    fn change_base_delay(&mut self, base_delay: Duration) {
        self.base_delay = base_delay;
        self.next_delay = self.base_delay;
    }

    fn reset(&mut self, current_time: Instant) {
        // While the external application has indicated success, we don't actually want to reset the
        // retry policy just yet. Instead, we want to record the time it was successful. Then when
        // we calculate the next delay, we can reset the strategy ONLY when it has been at least
        // DEFAULT_RESET_RETRY_INTERVAL seconds.
        self.good_since = Some(current_time);
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Add;
    use std::time::{Duration, Instant};

    use super::{BackoffRetry, RetryStrategy};

    #[test]
    fn test_fixed_retry() {
        let base = Duration::from_secs(10);
        let mut retry = BackoffRetry::new(base, Duration::from_secs(30), 1, false);
        let start = Instant::now() - Duration::from_secs(60);

        assert_eq!(retry.next_delay(start), base);
        assert_eq!(retry.next_delay(start.add(Duration::from_secs(1))), base);
        assert_eq!(retry.next_delay(start.add(Duration::from_secs(2))), base);
    }

    #[test]
    fn test_able_to_reset_base_delay() {
        let base = Duration::from_secs(10);
        let mut retry = BackoffRetry::new(base, Duration::from_secs(30), 1, false);
        let start = Instant::now();

        assert_eq!(retry.next_delay(start), base);
        assert_eq!(retry.next_delay(start.add(Duration::from_secs(1))), base);

        let base = Duration::from_secs(3);
        retry.change_base_delay(base);
        assert_eq!(retry.next_delay(start.add(Duration::from_secs(2))), base);
    }

    #[test]
    fn test_with_backoff() {
        let base = Duration::from_secs(10);
        let max = Duration::from_secs(60);
        let mut retry = BackoffRetry::new(base, max, 2, false);
        let start = Instant::now() - Duration::from_secs(60);

        assert_eq!(retry.next_delay(start), base);
        assert_eq!(
            retry.next_delay(start.add(Duration::from_secs(1))),
            base * 2
        );
        assert_eq!(
            retry.next_delay(start.add(Duration::from_secs(2))),
            base * 4
        );
        assert_eq!(retry.next_delay(start.add(Duration::from_secs(3))), max);
    }

    #[test]
    fn test_with_jitter() {
        let base = Duration::from_secs(10);
        let max = Duration::from_secs(60);
        let mut retry = BackoffRetry::new(base, max, 1, true);
        let start = Instant::now() - Duration::from_secs(60);

        let delay = retry.next_delay(start);
        assert!(base / 2 <= delay && delay <= base);
    }

    #[test]
    fn test_retry_holds_at_max() {
        let base = Duration::from_secs(20);
        let max = Duration::from_secs(30);

        let mut retry = BackoffRetry::new(base, max, 2, false);
        let start = Instant::now();
        retry.reset(start);

        let time = start.add(Duration::from_secs(20));
        let delay = retry.next_delay(time);
        assert_eq!(delay, base);

        let time = time.add(Duration::from_secs(20));
        let delay = retry.next_delay(time);
        assert_eq!(delay, max);

        let time = time.add(Duration::from_secs(20));
        let delay = retry.next_delay(time);
        assert_eq!(delay, max);
    }

    #[test]
    fn test_reset_interval() {
        let base = Duration::from_secs(10);
        let max = Duration::from_secs(60);
        let reset_interval = Duration::from_secs(45);

        // Prepare a retry strategy that has succeeded at a specific point.
        let mut retry = BackoffRetry::new(base, max, 2, false);
        retry.reset_interval = reset_interval;
        let start = Instant::now() - Duration::from_secs(60);
        retry.reset(start);

        // Verify that calculating the next delay returns as expected
        let time = start.add(Duration::from_secs(1));
        let delay = retry.next_delay(time);
        assert_eq!(delay, base);

        // Verify resetting the last known good time doesn't change the retry policy since it hasn't
        // exceeded the retry interval.
        let time = time.add(delay);
        retry.reset(time);

        let time = time.add(Duration::from_secs(10));
        let delay = retry.next_delay(time);
        assert_eq!(delay, base * 2);

        // And finally check that if we exceed the reset interval, the retry strategy will default
        // back to base.
        let time = time.add(delay);
        retry.reset(time);

        let time = time.add(reset_interval);
        let delay = retry.next_delay(time);
        assert_eq!(delay, base);
    }
}