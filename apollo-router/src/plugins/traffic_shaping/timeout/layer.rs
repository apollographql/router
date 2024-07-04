use std::time::Duration;

use tower::Layer;

use super::Timeout;

/// Applies a timeout to requests via the supplied inner service.
#[derive(Debug, Clone)]
pub(crate) struct TimeoutLayer {
    timeout: Duration,
}

impl TimeoutLayer {
    /// Create a timeout from a duration
    pub(crate) fn new(timeout: Duration) -> Self {
        TimeoutLayer { timeout }
    }
}

impl<S: Clone> Layer<S> for TimeoutLayer {
    type Service = Timeout<S>;

    fn layer(&self, service: S) -> Self::Service {
        Timeout::new(service, self.timeout)
    }
}
