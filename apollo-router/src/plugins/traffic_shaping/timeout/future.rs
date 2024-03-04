//! Future types

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use pin_project_lite::pin_project;
use tokio::time::Sleep;

use super::error::Elapsed;

pin_project! {
    /// [`Timeout`] response future
    ///
    /// [`Timeout`]: crate::timeout::Timeout
    #[derive(Debug)]
    pub(crate) struct ResponseFuture<T> {
        #[pin]
        response: T,
        #[pin]
        sleep: Pin<Box<Sleep>>,
    }
}

impl<T> ResponseFuture<T> {
    pub(crate) fn new(response: T, sleep: Pin<Box<Sleep>>) -> Self {
        ResponseFuture { response, sleep }
    }
}

impl<F, T, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<tower::BoxError>,
{
    type Output = Result<T, tower::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        // First, try polling the future
        match this.response.poll(cx) {
            Poll::Ready(v) => return Poll::Ready(v.map_err(Into::into)),
            Poll::Pending => {}
        }

        // Now check the sleep
        match Pin::new(&mut this.sleep).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(_) => {
                tracing::info!(monotonic_counter.apollo_router_timeout = 1u64,);
                Poll::Ready(Err(Elapsed::new().into()))
            }
        }
    }
}
