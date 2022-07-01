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
use futures::future::ready;
use futures::stream::once;
use futures::stream::BoxStream;
use http::header::HeaderName;
use http::header::{self};
use http::HeaderValue;
use http::Method;
use multimap::MultiMap;

use crate::graphql;

/// Temporary holder of header name while for use while building requests and responses. Required
/// because header name creation is fallible.
#[derive(Eq, Hash, PartialEq)]
pub enum IntoHeaderName {
    String(String),
    HeaderName(HeaderName),
}

/// Temporary holder of header value while for use while building requests and responses. Required
/// because header value creation is fallible.
#[derive(Eq, Hash, PartialEq)]
pub enum IntoHeaderValue {
    String(String),
    HeaderValue(HeaderValue),
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

/// Wrap an http Request.
#[derive(Debug)]
pub struct Request<T> {
    inner: http::Request<T>,
}

// Most of the required functionality is provided by our Deref and DerefMut implementations.
#[buildstructor::buildstructor]
impl<T> Request<T> {
    /// This is the constructor (or builder) to use when constructing a real Request.
    ///
    /// Required parameters are required in non-testing code to create a Request.
    #[builder]
    pub fn new(
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
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

    /// This is the constructor (or builder) to use when constructing a "fake" Request.
    ///
    /// This does not enforce the provision of the uri and method that is required for a fully functional
    /// Request. It's usually enough for testing, when a fully constructed Request is
    /// difficult to construct and not required for the purposes of the test.
    ///
    /// In addition, fake requests are expected to be valid, and will panic if given invalid values.
    #[builder]
    pub fn fake_new(
        headers: MultiMap<IntoHeaderName, IntoHeaderValue>,
        uri: Option<http::Uri>,
        method: Option<http::Method>,
        body: T,
    ) -> http::Result<Request<T>> {
        Self::new(
            headers,
            uri.unwrap_or_default(),
            method.unwrap_or(Method::GET),
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

impl<T> From<Request<T>> for http::Request<T> {
    fn from(request: Request<T>) -> http::Request<T> {
        request.inner
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

impl<T: Eq> Eq for Request<T> {}

/// Wrap an http Response.
#[derive(Debug, Default)]
pub struct Response<T> {
    pub inner: http::Response<T>,
}

impl<T> Response<T> {
    pub fn map<F, U>(self, f: F) -> Response<U>
    where
        F: FnMut(T) -> U,
    {
        self.inner.map(f).into()
    }
}

impl Response<BoxStream<'static, graphql::Response>> {
    pub fn from_response_to_stream(http: http::response::Response<graphql::Response>) -> Self {
        let (parts, body) = http.into_parts();
        Response {
            inner: http::Response::from_parts(parts, Box::pin(once(ready(body)))),
        }
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
