//! Wrapper types for [`http::Request`] and [`http::Response`] from the http crate.
//!
//! To improve their usability.

use std::cmp::PartialEq;
use std::hash::Hash;
use std::ops::Deref;
use std::ops::DerefMut;

use axum::body::boxed;
use axum::response::IntoResponse;
use bytes::Bytes;
use http::header;
use http::header::HeaderName;
use http::HeaderValue;
use multimap::MultiMap;

use crate::graphql;
use crate::services::APPLICATION_JSON_HEADER_VALUE;

/// Delayed-fallibility wrapper for conversion to [`http::header::HeaderName`].
///
/// `buildstructor` builders allow doing implict conversions for convenience,
/// but only infallible ones.
/// `HeaderName` can be converted from various types but the conversions is often fallible,
/// with `TryFrom` or `TryInto` instead of `From` or `Into`.
/// This types splits conversion in two steps:
/// it implements infallible conversion from various types like `&str` (that builders can rely on)
/// and fallible conversion to `HeaderName` (called later where we can handle errors).
///
/// See for example [`supergraph::Request::builder`][crate::services::supergraph::Request::builder]
/// which can be used like this:
///
/// ```
/// # fn main() -> Result<(), tower::BoxError> {
/// use apollo_router::services::supergraph;
/// let request = supergraph::Request::fake_builder()
///     .header("accept-encoding", "gzip")
///     // Other parameters
///     .build()?;
/// # Ok(()) }
/// ```
pub struct TryIntoHeaderName {
    /// The fallible conversion result
    result: Result<header::HeaderName, header::InvalidHeaderName>,
}

/// Delayed-fallibility wrapper for conversion to [`http::header::HeaderValue`].
///
/// `buildstructor` builders allow doing implict conversions for convenience,
/// but only infallible ones.
/// `HeaderValue` can be converted from various types but the conversions is often fallible,
/// with `TryFrom` or `TryInto` instead of `From` or `Into`.
/// This types splits conversion in two steps:
/// it implements infallible conversion from various types like `&str` (that builders can rely on)
/// and fallible conversion to `HeaderValue` (called later where we can handle errors).
///
/// See for example [`supergraph::Request::builder`][crate::services::supergraph::Request::builder]
/// which can be used like this:
///
/// ```
/// # fn main() -> Result<(), tower::BoxError> {
/// use apollo_router::services::supergraph;
/// let request = supergraph::Request::fake_builder()
///     .header("accept-encoding", "gzip")
///     // Other parameters
///     .build()?;
/// # Ok(()) }
/// ```
pub struct TryIntoHeaderValue {
    /// The fallible conversion result
    result: Result<header::HeaderValue, header::InvalidHeaderValue>,
}

impl TryFrom<TryIntoHeaderName> for header::HeaderName {
    type Error = header::InvalidHeaderName;

    fn try_from(value: TryIntoHeaderName) -> Result<Self, Self::Error> {
        value.result
    }
}

impl TryFrom<TryIntoHeaderValue> for header::HeaderValue {
    type Error = header::InvalidHeaderValue;

    fn try_from(value: TryIntoHeaderValue) -> Result<Self, Self::Error> {
        value.result
    }
}

impl From<header::HeaderName> for TryIntoHeaderName {
    fn from(value: header::HeaderName) -> Self {
        Self { result: Ok(value) }
    }
}

impl From<&'_ header::HeaderName> for TryIntoHeaderName {
    fn from(value: &'_ header::HeaderName) -> Self {
        Self {
            result: Ok(value.clone()),
        }
    }
}

impl From<&'_ [u8]> for TryIntoHeaderName {
    fn from(value: &'_ [u8]) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<&'_ str> for TryIntoHeaderName {
    fn from(value: &'_ str) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<Vec<u8>> for TryIntoHeaderName {
    fn from(value: Vec<u8>) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<String> for TryIntoHeaderName {
    fn from(value: String) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<header::HeaderValue> for TryIntoHeaderValue {
    fn from(value: header::HeaderValue) -> Self {
        Self { result: Ok(value) }
    }
}

impl From<&'_ header::HeaderValue> for TryIntoHeaderValue {
    fn from(value: &'_ header::HeaderValue) -> Self {
        Self {
            result: Ok(value.clone()),
        }
    }
}

impl From<&'_ [u8]> for TryIntoHeaderValue {
    fn from(value: &'_ [u8]) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<&'_ str> for TryIntoHeaderValue {
    fn from(value: &'_ str) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<Vec<u8>> for TryIntoHeaderValue {
    fn from(value: Vec<u8>) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl From<String> for TryIntoHeaderValue {
    fn from(value: String) -> Self {
        Self {
            result: value.try_into(),
        }
    }
}

impl Eq for TryIntoHeaderName {}

impl PartialEq for TryIntoHeaderName {
    fn eq(&self, other: &Self) -> bool {
        match (&self.result, &other.result) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        }
    }
}

impl Hash for TryIntoHeaderName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match &self.result {
            Ok(value) => value.hash(state),
            Err(_) => {}
        }
    }
}

pub(crate) fn header_map(
    from: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
) -> Result<http::HeaderMap<http::HeaderValue>, http::Error> {
    let mut http = http::HeaderMap::new();
    for (key, values) in from {
        let name = key.result?;
        let mut values = values.into_iter();
        if let Some(last) = values.next_back() {
            for value in values {
                http.append(name.clone(), value.result?);
            }
            http.append(name, last.result?);
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
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
        uri: http::Uri,
        method: http::Method,
        body: T,
    ) -> http::Result<Request<T>> {
        let mut builder = http::request::Builder::new().method(method).uri(uri);
        for (key, values) in headers {
            let header_name: HeaderName = key.try_into()?;
            for value in values {
                let header_value: HeaderValue = value.try_into()?;
                builder = builder.header(header_name.clone(), header_value);
            }
        }
        let req = builder.body(body)?;
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
        headers: MultiMap<TryIntoHeaderName, TryIntoHeaderValue>,
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
        parts
            .headers
            .insert(header::CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());

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
