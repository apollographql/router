//! Wrapper types for [`http::Request`] and [`http::Response`] from the http crate.
//!
//! To improve their usability.

use std::{
    cmp::PartialEq,
    hash::Hash,
    ops::{Deref, DerefMut},
};

#[cfg(feature = "axum-server")]
use axum::{body::boxed, response::IntoResponse};
#[cfg(feature = "axum-server")]
use bytes::Bytes;

#[cfg(feature = "axum-server")]
use crate::ResponseBody;

use http::{
    header::HeaderName,
    request::{Builder, Parts},
    uri::InvalidUri,
    HeaderValue, Version,
};

#[derive(Debug)]
pub struct Request<T> {
    inner: http::Request<T>,
}

// Most of the required functionality is provided by our Deref and DerefMut implementations.
impl<T> Request<T> {
    /// Update the associated URL
    pub fn from_parts(head: Parts, body: T) -> Request<T> {
        Request {
            inner: http::Request::from_parts(head, body),
        }
    }

    /// Consumes the request, returning just the body.
    pub fn into_body(self) -> T {
        self.inner.into_body()
    }

    /// Consumes the request returning the head and body parts.
    pub fn into_parts(self) -> (http::request::Parts, T) {
        self.inner.into_parts()
    }

    /// Consumes the request returning a new request with body mapped to the return type of the passed in function.
    pub fn map<F, U>(self, f: F) -> Result<Request<U>, InvalidUri>
    where
        F: FnOnce(T) -> U,
    {
        Ok(Request {
            inner: self.inner.map(f),
        })
    }
}

impl<T> Request<T>
where
    T: Default,
{
    // Only used for plugin::utils and tests
    pub fn mock() -> Request<T> {
        Request {
            inner: http::Request::default(),
        }
    }
}

impl<T> Deref for Request<T> {
    type Target = http::Request<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for Request<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T: Clone> Clone for Request<T> {
    fn clone(&self) -> Self {
        // note: we cannot clone the extensions because we cannot know
        // which types were stored
        let mut req = http::Request::builder()
            .method(self.inner.method().clone())
            .version(self.inner.version())
            .uri(self.inner.uri().clone());
        req.headers_mut().unwrap().extend(
            self.inner
                .headers()
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );

        let req = req
            .body(self.inner.body().clone())
            .expect("cloning a valid request creates a valid request");
        Self { inner: req }
    }
}

impl<T: Hash> Hash for Request<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.method().hash(state);
        self.inner.version().hash(state);
        self.inner.uri().hash(state);
        // this assumes headers are in the same order
        for (name, value) in self.inner.headers() {
            name.hash(state);
            value.hash(state);
        }
        self.inner.body().hash(state);
    }
}

impl<T: PartialEq> PartialEq for Request<T> {
    fn eq(&self, other: &Self) -> bool {
        let mut res = self.inner.method().eq(other.inner.method())
            && self.inner.version().eq(&other.inner.version())
            && self.inner.uri().eq(other.inner.uri());

        if !res {
            return false;
        }
        if self.inner.headers().len() != other.inner.headers().len() {
            return false;
        }

        // this assumes headers are in the same order
        for ((name, value), (other_name, other_value)) in self
            .inner
            .headers()
            .iter()
            .zip(other.inner.headers().iter())
        {
            res = name.eq(other_name) && value.eq(other_value);
            if !res {
                return false;
            }
        }

        self.inner.body().eq(other.inner.body())
    }
}

impl<T: PartialEq> Eq for Request<T> {}

impl<T> From<Request<T>> for http::Request<T> {
    fn from(request: Request<T>) -> Self {
        request.inner
    }
}

impl<T> TryFrom<http::Request<T>> for Request<T> {
    type Error = InvalidUri;
    fn try_from(request: http::Request<T>) -> Result<Self, Self::Error> {
        Ok(Self { inner: request })
    }
}

#[derive(Debug)]
pub struct RequestBuilder {
    inner: Builder,
}

impl RequestBuilder {
    pub fn new(method: http::method::Method, uri: http::Uri) -> Self {
        // Enforce the need for a method and an uri
        let builder = Builder::new().method(method).uri(uri);
        Self { inner: builder }
    }

    /// Set the HTTP version for this request.
    pub fn version(self, version: Version) -> Self {
        Self {
            inner: self.inner.version(version),
        }
    }

    /// Appends a header to this request builder.
    pub fn header<K, V>(self, key: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        Self {
            inner: self.inner.header(key, value),
        }
    }

    /// "Consumes" this builder, using the provided `body` to return a
    /// constructed `Request`.
    pub fn body<T>(self, body: T) -> http::Result<Request<T>> {
        Ok(Request {
            inner: self.inner.body(body)?,
        })
    }
}

#[derive(Debug, Default)]
pub struct Response<T> {
    pub inner: http::Response<T>,
}

impl<T> Response<T> {
    pub fn into_parts(self) -> (http::response::Parts, T) {
        self.inner.into_parts()
    }

    pub fn into_body(self) -> T {
        self.inner.into_body()
    }

    pub fn map<F, U>(self, f: F) -> Response<U>
    where
        F: FnOnce(T) -> U,
    {
        self.inner.map(f).into()
    }
}

impl<T> Deref for Response<T> {
    type Target = http::Response<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for Response<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> From<http::Response<T>> for Response<T> {
    fn from(inner: http::Response<T>) -> Self {
        Response { inner }
    }
}

impl<T> From<Response<T>> for http::Response<T> {
    fn from(response: Response<T>) -> http::Response<T> {
        response.inner
    }
}

impl<T: Clone> Clone for Response<T> {
    fn clone(&self) -> Self {
        // note: we cannot clone the extensions because we cannot know
        // which types were stored
        let mut res = http::Response::builder()
            .status(self.inner.status())
            .version(self.inner.version());
        res.headers_mut().unwrap().extend(
            self.inner
                .headers()
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );

        let res = res
            .body(self.inner.body().clone())
            .expect("cloning a valid response creates a valid response");
        Self { inner: res }
    }
}

pub fn convert_uri(uri: http::Uri) -> Result<url::Url, url::ParseError> {
    url::Url::parse(&uri.to_string())
}

#[cfg(feature = "axum-server")]
impl IntoResponse for Response<ResponseBody> {
    fn into_response(self) -> axum::response::Response {
        // todo: chunks?
        let (parts, body) = self.into_parts();
        let json_body_bytes =
            Bytes::from(serde_json::to_vec(&body).expect("body should be serializable; qed"));

        axum::response::Response::from_parts(parts, boxed(http_body::Full::new(json_body_bytes)))
    }
}

#[cfg(feature = "axum-server")]
impl IntoResponse for Response<Bytes> {
    fn into_response(self) -> axum::response::Response {
        // todo: chunks?
        let (parts, body) = self.into_parts();

        axum::response::Response::from_parts(parts, boxed(http_body::Full::new(body)))
    }
}
