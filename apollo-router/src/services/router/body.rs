#![allow(deprecated)]
use std::fmt::Debug;

use bytes::Buf;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::Stream;
use http_body::SizeHint;
use http_body_util::combinators::BoxBody;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use hyper::body::Body as HttpBody;
use tower::BoxError;
use tower::Service;

pub type RouterBody = BoxBody<Bytes, hyper::Error>;

pub(crate) async fn get_body_bytes<B: HttpBody>(body: B) -> Result<Bytes, B::Error> {
    Ok(body
        .collect()
        .await?
        // .map_err(axum::Error::new)?
        .to_bytes())
}

// We create some utility functions to make Empty and Full bodies
// and convert types
pub(crate) fn empty() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

pub(crate) fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

pub(crate) fn wrap_body_as_data_stream(body: RouterBody) -> http_body_util::BodyDataStream<Bytes> {
    http_body_util::BodyDataStream::new(body)
}

pub(crate) fn wrap_stream_as_stream_body<S, D, E>(stream: S) -> http_body_util::StreamBody<D>
where
    S: Stream<Item = Result<http_body::Frame<D>, E>>,
    D: Buf,
{
    http_body_util::StreamBody::new(stream)
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
