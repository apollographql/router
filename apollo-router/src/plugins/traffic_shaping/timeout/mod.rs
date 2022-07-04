//! This is a modified Timeout service copy/pasted from the tower codebase.
//! This Timeout is also checking if we do not timeout on the `poll_ready` and not only on the `call` part
//! Middleware that applies a timeout to requests.
//!
//! If the response does not complete within the specified timeout, the response
//! will be aborted.

pub(crate) mod error;
pub(crate) mod future;
mod layer;

use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use futures::Future;
use tokio::time::Instant;
use tokio::time::Sleep;
use tower::Service;

use self::future::ResponseFuture;
pub(crate) use self::layer::TimeoutLayer;
pub(crate) use crate::plugins::traffic_shaping::timeout::error::Elapsed;

/// Applies a timeout to requests.
#[derive(Debug)]
pub(crate) struct Timeout<T> {
    inner: T,
    timeout: Duration,
    sleep: Option<Pin<Box<Sleep>>>,
}

// ===== impl Timeout =====

impl<T> Timeout<T> {
    /// Creates a new [`Timeout`]
    pub(crate) fn new(inner: T, timeout: Duration) -> Self {
        Timeout {
            inner,
            timeout,
            // The sleep won't actually be used with this duration, but
            // we create it eagerly so that we can reset it in place rather than
            // `Box::pinning` a new `Sleep` every time we need one.
            sleep: None,
        }
    }
}

impl<S, Request> Service<Request> for Timeout<S>
where
    S: Service<Request>,
    S::Error: Into<tower::BoxError>,
{
    type Response = S::Response;
    type Error = tower::BoxError;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.sleep.is_none() {
            self.sleep = Some(Box::pin(tokio::time::sleep_until(
                Instant::now() + self.timeout,
            )));
        }
        match self.inner.poll_ready(cx) {
            Poll::Pending => {}
            Poll::Ready(r) => return Poll::Ready(r.map_err(Into::into)),
        };

        // Checking if we don't timeout on `poll_ready`
        if Pin::new(
            &mut self
                .sleep
                .as_mut()
                .expect("we can unwrap because we set it just before"),
        )
        .poll(cx)
        .is_ready()
        {
            tracing::trace!("timeout exceeded.");
            self.sleep = None;

            return Poll::Ready(Err(Elapsed::new().into()));
        }

        Poll::Pending
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let response = self.inner.call(request);

        ResponseFuture::new(
            response,
            self.sleep
                .take()
                .expect("poll_ready must been called before"),
        )
    }
}
