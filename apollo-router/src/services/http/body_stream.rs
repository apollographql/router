use std::task::Poll;

use futures::Stream;
use pin_project_lite::pin_project;

pin_project! {
    /// Allows conversion between an http_body::Body and a futures stream.
    pub(crate) struct BodyStream<B: http_body::Body> {
        #[pin]
        inner: B
    }
}

impl<B: hyper::body::HttpBody> BodyStream<B> {
    /// Create a new `BodyStream`.
    pub(crate) fn new(body: B) -> Self {
        Self { inner: body }
    }
}

impl<B, D, E> Stream for BodyStream<B>
where
    B: http_body::Body<Data = D, Error = E>,
    B::Error: Into<E>,
{
    type Item = Result<D, E>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.project().inner.poll_data(cx)
    }
}
