//! This is a modified Timeout service copy/pasted from the tower codebase.
//! This Timeout is also checking if we do not timeout on the `poll_ready` and not only on the `call` part
//! Middleware that applies a timeout to requests.
//!
//! If the response does not complete within the specified timeout, the response
//! will be aborted.

pub(crate) mod error;
pub(crate) mod future;
mod layer;

use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use tower::util::Oneshot;
use tower::Service;
use tower::ServiceExt;

use self::future::ResponseFuture;
pub(crate) use self::layer::TimeoutLayer;
pub(crate) use crate::plugins::traffic_shaping::timeout::error::Elapsed;

/// Applies a timeout to requests.
#[derive(Debug, Clone)]
pub(crate) struct Timeout<T: Clone> {
    inner: T,
    timeout: Duration,
}

// ===== impl Timeout =====

impl<T: Clone> Timeout<T> {
    /// Creates a new [`Timeout`]
    pub(crate) fn new(inner: T, timeout: Duration) -> Self {
        Timeout { inner, timeout }
    }
}

impl<S, Request> Service<Request> for Timeout<S>
where
    S: Service<Request> + Clone,
    S::Error: Into<tower::BoxError>,
{
    type Response = S::Response;
    type Error = tower::BoxError;
    type Future = ResponseFuture<Oneshot<S, Request>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let service = self.inner.clone();

        let response = service.oneshot(request);

        ResponseFuture::new(response, Box::pin(tokio::time::sleep(self.timeout)))
    }
}
