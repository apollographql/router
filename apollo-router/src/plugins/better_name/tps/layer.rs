use std::num::NonZeroU64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tower::Layer;

use super::service::TpsLimit;
use super::tps::Tps;

/// Enforces a rate limit on the number of requests the underlying
/// service can handle over a period of time.
#[derive(Debug, Clone)]
pub(crate) struct TpsLimitLayer {
    tps: Tps,
    window_start: Arc<AtomicU64>,
    previous_nb_requests: Arc<AtomicUsize>,
    current_nb_requests: Arc<AtomicUsize>,
}

impl TpsLimitLayer {
    /// Create new tps limit layer.
    pub(crate) fn new(capacity: NonZeroU64, interval: Duration) -> Self {
        let tps = Tps::new(capacity, interval);
        TpsLimitLayer {
            tps,
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

impl<S> Layer<S> for TpsLimitLayer {
    type Service = TpsLimit<S>;

    fn layer(&self, service: S) -> Self::Service {
        TpsLimit {
            inner: service,
            tps: self.tps,
            window_start: self.window_start.clone(),
            previous_nb_requests: self.previous_nb_requests.clone(),
            current_nb_requests: self.current_nb_requests.clone(),
        }
    }
}
