#![allow(deprecated)]
use std::fmt::Debug;

use bytes::Bytes;
use futures::Stream;
use hyper::body::HttpBody;

pub struct RouterBody(pub(crate) super::Body);

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
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut pinned = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        pinned.as_mut().poll_next(cx)
    }
}

impl HttpBody for RouterBody {
    type Data = <hyper::body::Body as HttpBody>::Data;

    type Error = <hyper::body::Body as HttpBody>::Error;

    fn poll_data(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Self::Data, Self::Error>>> {
        let mut pinned = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        pinned.as_mut().poll_data(cx)
    }

    fn poll_trailers(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        let mut pinned = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        pinned.as_mut().poll_trailers(cx)
    }
}
