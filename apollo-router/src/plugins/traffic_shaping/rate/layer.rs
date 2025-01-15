use std::num::NonZeroU64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tower::Layer;

use super::Rate;
use super::RateLimit;
use super::RateLimiting;

/// Who configured the rate limit, the User or Apollo
#[derive(Debug, Clone)]
pub(crate) enum ConfiguredBy {
    Apollo,
    User,
}

/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
#[derive(Debug, Clone)]
pub(crate) struct RateLimitLayer {
    configured_by: ConfiguredBy,
    rate: Rate,
    window_start: Arc<AtomicU64>,
    previous_nb_requests: Arc<AtomicUsize>,
    current_nb_requests: Arc<AtomicUsize>,
}

impl RateLimitLayer {
    /// Create new rate limit layer.
    pub(crate) fn new(num: NonZeroU64, per: Duration, configured_by: ConfiguredBy) -> Self {
        let rate = Rate::new(num, per);
        Self {
            configured_by,
            rate,
            window_start: Arc::new(AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time must be after EPOCH")
                    .as_millis() as u64,
            )),
            previous_nb_requests: Arc::default(),
            current_nb_requests: Arc::new(AtomicUsize::new(1)),
        }
    }
}
impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimiting<S>;

    fn layer(&self, service: S) -> Self::Service {
        match self.configured_by {
            ConfiguredBy::Apollo => RateLimiting::Apollo(RateLimit {
                inner: service,
                rate: self.rate,
                window_start: self.window_start.clone(),
                previous_nb_requests: self.previous_nb_requests.clone(),
                current_nb_requests: self.current_nb_requests.clone(),
            }),
            ConfiguredBy::User => RateLimiting::User(RateLimit {
                inner: service,
                rate: self.rate,
                window_start: self.window_start.clone(),
                previous_nb_requests: self.previous_nb_requests.clone(),
                current_nb_requests: self.current_nb_requests.clone(),
            }),
        }
    }
}
