use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use tokio::time::Instant;
use tokio::time::Sleep;
use tower::Layer;

use super::service::State;
use super::Rate;
use super::RateLimit;

/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
#[derive(Debug, Clone)]
pub(crate) struct RateLimitLayer {
    rate: Rate,
    state: Arc<RwLock<State>>,
    sleep: Arc<RwLock<Pin<Box<Sleep>>>>,
}

impl RateLimitLayer {
    /// Create new rate limit layer.
    pub(crate) fn new(num: u64, per: Duration) -> Self {
        let rate = Rate::new(num, per);
        let until = Instant::now();
        let state = State::Ready {
            until,
            rem: rate.num(),
        };

        RateLimitLayer {
            rate,
            sleep: Arc::new(RwLock::new(Box::pin(tokio::time::sleep_until(until)))),
            state: Arc::new(RwLock::new(state)),
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimit<S>;

    fn layer(&self, service: S) -> Self::Service {
        RateLimit {
            inner: service,
            rate: self.rate,
            state: self.state.clone(),
            sleep: self.sleep.clone(),
        }
    }
}
