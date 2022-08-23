//! Wrapper types for [`http::Request`] and [`http::Response`] from the http crate.
//!
//! To improve their usability.

#![allow(missing_docs)] // FIXME

use std::cmp::PartialEq;
use std::hash::Hash;
use std::ops::Deref;
use std::ops::DerefMut;

use axum::body::boxed;
use axum::response::IntoResponse;
use bytes::Bytes;
use http::header::HeaderName;
use http::header::{self};
use http::HeaderValue;
use multimap::MultiMap;

use crate::graphql;

/// Temporary holder of header name while for use while building requests and responses. Required
/// because header name creation is fallible.
#[derive(Eq)]
pub enum IntoHeaderName {
    String(String),
    HeaderName(HeaderName),
}

/// Temporary holder of header value while for use while building requests and responses. Required
/// because header value creation is fallible.
#[derive(Eq)]
pub enum IntoHeaderValue {
    String(String),
    HeaderValue(HeaderValue),
}

impl PartialEq for IntoHeaderName {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq for IntoHeaderValue {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Hash for IntoHeaderName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl Hash for IntoHeaderValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl IntoHeaderName {
    fn as_bytes(&self) -> &[u8] {
        match self {
            IntoHeaderName::String(s) => s.as_bytes(),
            IntoHeaderName::HeaderName(h) => h.as_str().as_bytes(),
        }
    }
}

impl IntoHeaderValue {
    fn as_bytes(&self) -> &[u8] {
        match self {
            IntoHeaderValue::String(s) => s.as_bytes(),
            IntoHeaderValue::HeaderValue(v) => v.as_bytes(),
        }
    }
}

impl<T> From<T> for IntoHeaderName
where
    T: std::fmt::Display,
{
    fn from(name: T) -> Self {
        IntoHeaderName::String(name.to_string())
    }
}

impl<T> From<T> for IntoHeaderValue
where
    T: std::fmt::Display,
{
    fn from(name: T) -> Self {
        IntoHeaderValue::String(name.to_string())
    }
}

impl TryFrom<IntoHeaderName> for HeaderName {
    type Error = http::Error;

    fn try_from(value: IntoHeaderName) -> Result<Self, Self::Error> {
        Ok(match value {
            IntoHeaderName::String(name) => HeaderName::try_from(name)?,
            IntoHeaderName::HeaderName(name) => name,
        })
    }
}

impl TryFrom<IntoHeaderValue> for HeaderValue {
    type Error = http::Error;

    fn try_from(value: IntoHeaderValue) -> Result<Self, Self::Error> {
        Ok(match value {
            IntoHeaderValue::String(value) => HeaderValue::try_from(value)?,
            IntoHeaderValue::HeaderValue(value) => value,
        })
    }
}

pub(crate) fn header_map(
    from: MultiMap<IntoHeaderName, IntoHeaderValue>,
) -> Result<http::HeaderMap<http::HeaderValue>, http::Error> {
    let mut http = http::HeaderMap::new();
    for (key, values) in from {
        let name = http::header::HeaderName::try_from(key)?;
        for value in values {
            http.append(name.clone(), value.try_into()?);
        }
    }
    Ok(http)
}

/// Ignores `http::Extensions`
pub(crate) fn clone_http_request<B: Clone>(request: &http::Request<B>) -> http::Request<B> {
    let mut new = http::Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version())
        .body(request.body().clone())
        .unwrap();
    *new.headers_mut() = request.headers().clone();
    new
}

/// Ignores `http::Extensions`
pub(crate) fn clone_http_response<B: Clone>(response: &http::Response<B>) -> http::Response<B> {
    let mut new = http::Response::builder()
        .status(response.status())
        .version(response.version())
        .body(response.body().clone())
        .unwrap();
    *new.headers_mut() = response.headers().clone();
    new
}

/// Wrap an http Request.
#[derive(Debug)]
pub(crate) struct Request<T> {
    pub(crate) inner: http::Request<T>,
}

// Most of the required functionality is provided by our Deref and DerefMut implementations.
#[buildstructor::buildstructor]
impl<T> Request<T> {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder]
    pub(crate) fn new(
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        uri: http::Uri,
        method: http::Method,
        body: T,
    ) -> http::Result<Request<T>> {
        let mut req = http::request::Builder::new()
            .method(method)
            .uri(uri)
            .body(body)?;
        *req.headers_mut() = header_map(headers)?;
        Ok(Self { inner: req })
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl<T> Request<T> {
    /// This is the constructor (or builder) to use when constructing a "fake" Request.
    ///
    /// This does not enforce the provision of the uri and method that is required for a fully functional
    /// Request. It's usually enough for testing, when a fully constructed Request is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake requests are expected to be valid, and will panic if given invalid values.
    #[builder]
    pub(crate) fn fake_new(
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        uri: Option<http::Uri>,
        method: Option<http::Method>,
        body: T,
    ) -> http::Result<Request<T>> {
        Self::new(
            headers,
            uri.unwrap_or_default(),
            method.unwrap_or(http::Method::GET),
            body,
        )
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

impl<T> From<http::Request<T>> for Request<T> {
    fn from(inner: http::Request<T>) -> Self {
        Request { inner }
    }
}

impl<T: Clone> From<&'_ http::Request<T>> for Request<T> {
    fn from(req: &'_ http::Request<T>) -> Self {
        Request {
            inner: clone_http_request(req),
        }
    }
}

impl<T> From<Request<T>> for http::Request<T> {
    fn from(request: Request<T>) -> http::Request<T> {
        request.inner
    }
}

impl<T: Clone> Clone for Request<T> {
    fn clone(&self) -> Self {
        Self {
            inner: clone_http_request(&self.inner),
        }
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

impl<T: Eq> Eq for Request<T> {}

/// Wrap an http Response.
#[derive(Debug, Default)]
pub(crate) struct Response<T> {
    pub(crate) inner: http::Response<T>,
}

#[cfg(test)]
pub(crate) fn from_response_to_stream(
    http: http::response::Response<graphql::Response>,
) -> http::Response<futures::stream::BoxStream<'static, graphql::Response>> {
    use futures::future::ready;
    use futures::stream::once;
    use futures::StreamExt;

    http.map(|body| once(ready(body)).boxed())
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

impl<T: Clone> From<&'_ http::Response<T>> for Response<T> {
    fn from(req: &'_ http::Response<T>) -> Self {
        Response {
            inner: clone_http_response(req),
        }
    }
}

impl<T> From<Response<T>> for http::Response<T> {
    fn from(response: Response<T>) -> http::Response<T> {
        response.inner
    }
}

impl<T: Clone> Clone for Response<T> {
    fn clone(&self) -> Self {
        Self {
            inner: clone_http_response(&self.inner),
        }
    }
}

impl IntoResponse for Response<graphql::Response> {
    fn into_response(self) -> axum::response::Response {
        // todo: chunks?
        let (mut parts, body) = http::Response::from(self).into_parts();
        let json_body_bytes =
            Bytes::from(serde_json::to_vec(&body).expect("body should be serializable; qed"));
        parts.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        axum::response::Response::from_parts(parts, boxed(http_body::Full::new(json_body_bytes)))
    }
}

impl IntoResponse for Response<Bytes> {
    fn into_response(self) -> axum::response::Response {
        // todo: chunks?
        let (parts, body) = http::Response::from(self).into_parts();

        axum::response::Response::from_parts(parts, boxed(http_body::Full::new(body)))
    }
}

#[cfg(test)]
mod test {
    use http::HeaderValue;
    use http::Method;
    use http::Uri;

    use crate::http_ext::Request;

    #[test]
    fn builder() {
        let request = Request::builder()
            .header("a", "b")
            .header("a", "c")
            .uri(Uri::from_static("http://example.com"))
            .method(Method::POST)
            .body("test")
            .build()
            .unwrap();
        assert_eq!(
            request
                .headers()
                .get_all("a")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![HeaderValue::from_static("b"), HeaderValue::from_static("c")]
        );
        assert_eq!(request.uri(), &Uri::from_static("http://example.com"));
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.body(), &"test");
    }
}
