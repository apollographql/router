//! Future types

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use pin_project_lite::pin_project;

pin_project! {
    #[derive(Debug)]
    pub(crate) struct ResponseFuture<T> {
        #[pin]
        response: T,
    }
}

impl<T> ResponseFuture<T> {
    pub(crate) fn new(response: T) -> Self {
        ResponseFuture { response }
    }
}

impl<F, T, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<tower::BoxError>,
{
    type Output = Result<T, tower::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match this.response.poll(cx) {
            Poll::Ready(v) => Poll::Ready(v.map_err(Into::into)),
            Poll::Pending => Poll::Pending,
        }
    }
}
