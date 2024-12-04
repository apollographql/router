#![allow(deprecated)]
use std::fmt::Debug;

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::Stream;
use futures::StreamExt;
use http_body::Frame;
use http_body::SizeHint;
// use http_body_util::combinators::BoxBody;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyDataStream;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use http_body_util::StreamBody;
use hyper::body::Body as HttpBody;
use tower::BoxError;
use tower::Service;

pub type RouterBody = UnsyncBoxBody<Bytes, BoxError>;

pub(crate) async fn get_body_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    Ok(body.collect().await?.to_bytes())
}

// We create some utility functions to make Empty and Full bodies
// and convert types
pub(crate) fn empty() -> UnsyncBoxBody<Bytes, BoxError> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub(crate) fn full<T: Into<Bytes>>(chunk: T) -> UnsyncBoxBody<Bytes, BoxError> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

// Useful Conversion notes:
//  - If you have a body and want to convert it to BodyDataStream
//    You can call `body.into_data_stream` from BodyExt
//  - If you have a Stream and want a StreamBody, you can call
//    `StreamBody::new(stream)`.
//
//  I'll leave these functions here as examples and at some point
//  in the upgrade we can remove them.
//

pub(crate) fn into_data_stream_body(
    body: RouterBody,
) -> http_body_util::BodyDataStream<RouterBody> {
    body.into_data_stream()
}

pub(crate) fn into_stream_body<S, E>(stream: S) -> http_body_util::StreamBody<S>
where
    S: Stream<Item = Result<Frame<RouterBody>, E>>,
{
    http_body_util::StreamBody::new(stream)
}

pub(crate) fn from_data_stream(data_stream: BodyDataStream<RouterBody>) -> RouterBody {
    RouterBody::new(StreamBody::new(
        data_stream.map(|s| s.map(|body| Frame::data(body))),
    ))
}

pub(crate) fn from_result_stream<S>(data_stream: S) -> RouterBody
where
    S: Stream<Item = Result<Bytes, BoxError>> + Send + 'static,
{
    RouterBody::new(StreamBody::new(
        data_stream.map(|s| s.map(|body| Frame::data(body))),
    ))
}

// pub struct RouterBody(super::Body);

/*
impl RouterBody {
    pub fn empty() -> Self {
        Self(super::Body::empty())
    }

    pub fn into_inner(self) -> super::Body {
        self.0
    }

    pub async fn to_bytes(self) -> Result<Bytes, hyper::Error> {
        hyper::body::to_bytes(self.0).await
    }

    pub fn wrap_stream<S, O, E>(stream: S) -> RouterBody
    where
        S: Stream<Item = Result<O, E>> + Send + 'static,
        O: Into<Bytes> + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    {
        Self(super::Body::wrap_stream(stream))
    }
}

impl<T: Into<super::Body>> From<T> for RouterBody {
    fn from(value: T) -> Self {
        RouterBody(value.into())
    }
}

impl Debug for RouterBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Stream for RouterBody {
    type Item = <hyper::body::Body as Stream>::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut pinned = std::pin::pin!(&mut self.0);
        pinned.as_mut().poll_next(cx)
    }
}

impl HttpBody for RouterBody {
    type Data = <hyper::body::Body as HttpBody>::Data;

    type Error = <hyper::body::Body as HttpBody>::Error;

    fn poll_data(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Self::Data, Self::Error>>> {
        let mut pinned = std::pin::pin!(&mut self.0);
        pinned.as_mut().poll_data(cx)
    }

    fn poll_trailers(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        let mut pinned = std::pin::pin!(&mut self.0);
        pinned.as_mut().poll_trailers(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        HttpBody::size_hint(&self.0)
    }
}

pub(crate) async fn get_body_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    hyper::body::to_bytes(body).await
}
*/
