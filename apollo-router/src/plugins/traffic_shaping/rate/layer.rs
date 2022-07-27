use std::num::NonZeroU64;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use tokio::time::Instant;
use tower::Layer;

use super::Rate;
use super::RateLimit;
/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
#[derive(Debug, Clone)]
pub(crate) struct RateLimitLayer {
    rate: Rate,
    curr_time: Arc<RwLock<Instant>>,
    previous_counter: Arc<AtomicUsize>,
    current_counter: Arc<AtomicUsize>,
}

impl RateLimitLayer {
    /// Create new rate limit layer.
    pub(crate) fn new(num: NonZeroU64, per: Duration) -> Self {
        let rate = Rate::new(num, per);
        RateLimitLayer {
            rate,
            curr_time: Arc::new(RwLock::new(Instant::now())),
            previous_counter: Arc::default(),
            current_counter: Arc::new(AtomicUsize::new(1)),
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimit<S>;

    fn layer(&self, service: S) -> Self::Service {
        RateLimit {
            inner: service,
            rate: self.rate,
            curr_time: self.curr_time.clone(),
            previous_counter: self.previous_counter.clone(),
            current_counter: self.current_counter.clone(),
        }
    }
}
