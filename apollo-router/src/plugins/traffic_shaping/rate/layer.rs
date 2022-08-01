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
    window_start: Arc<RwLock<Instant>>,
    previous_nb_requests: Arc<AtomicUsize>,
    current_nb_requests: Arc<AtomicUsize>,
}

impl RateLimitLayer {
    /// Create new rate limit layer.
    pub(crate) fn new(num: NonZeroU64, per: Duration) -> Self {
        let rate = Rate::new(num, per);
        RateLimitLayer {
            rate,
            window_start: Arc::new(RwLock::new(Instant::now())),
            previous_nb_requests: Arc::default(),
            current_nb_requests: Arc::new(AtomicUsize::new(1)),
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimit<S>;

    fn layer(&self, service: S) -> Self::Service {
        RateLimit {
            inner: service,
            rate: self.rate,
            window_start: self.window_start.clone(),
            previous_nb_requests: self.previous_nb_requests.clone(),
            current_nb_requests: self.current_nb_requests.clone(),
        }
    }
}
