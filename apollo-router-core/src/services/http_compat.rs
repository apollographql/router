//! wrapper typpes for Request and Response from the http crate to improve their usability

use std::{
    cmp::PartialEq,
    hash::Hash,
    ops::{Deref, DerefMut},
    str::FromStr,
};

use http::{
    header::HeaderName, request::Builder, uri::InvalidUri, HeaderMap, HeaderValue, Method, Uri,
    Version,
};

#[derive(Debug)]
pub struct Request<T> {
    // The goal of having a copy of the url is to keep the right type for `ReqwestSubgraphService` and avoid re-parsing.
    // This url will stay the same than the uri in inner because we only can set a new url with `set_url` method
    pub(super) url: Uri,
    inner: http::Request<T>,
}

impl<T> Request<T> {
    /// Update the associated URL
    pub fn set_url(&mut self, url: http::Uri) -> Result<(), http::Error> {
        *self.inner.uri_mut() = url.clone();
        self.url = url;
        Ok(())
    }

    /// Returns a reference to the associated URL.
    pub fn url(&self) -> &http::Uri {
        &self.url
    }

    /// Returns a reference to the associated HTTP method.
    pub fn method(&self) -> &Method {
        self.inner.method()
    }

    /// Returns a mutable reference to the associated HTTP method.
    pub fn method_mut(&mut self) -> &mut Method {
        self.inner.method_mut()
    }

    /// Returns the associated version.
    pub fn version(&self) -> Version {
        self.inner.version()
    }

    /// Returns a mutable reference to the associated version.
    pub fn version_mut(&mut self) -> &mut Version {
        self.inner.version_mut()
    }

    /// Returns a reference to the associated header field map.
    pub fn headers(&self) -> &HeaderMap<HeaderValue> {
        self.inner.headers()
    }

    /// Returns a mutable reference to the associated header field map.
    pub fn headers_mut(&mut self) -> &mut HeaderMap<HeaderValue> {
        self.inner.headers_mut()
    }

    /// Returns a reference to the associated HTTP body.
    pub fn body(&self) -> &T {
        self.inner.body()
    }

    /// Returns a mutable reference to the associated HTTP body.
    pub fn body_mut(&mut self) -> &mut T {
        self.inner.body_mut()
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
        let new_req = self.inner.map(f);
        Ok(Request {
            url: new_req.uri().clone(),
            inner: new_req,
        })
    }
}

impl<T> Request<T>
where
    T: Default,
{
    // Only used for plugin_utils and tests
    pub fn mock() -> Request<T> {
        Request {
            url: Uri::from_str("http://default").unwrap(),
            inner: http::Request::default(),
        }
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
        Self {
            inner: req,
            url: self.url.clone(),
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

impl<T: PartialEq> Eq for Request<T> {}

impl<T> From<Request<T>> for http::Request<T> {
    fn from(request: Request<T>) -> Self {
        request.inner
    }
}

#[derive(Debug)]
pub struct RequestBuilder {
    url: http::Uri,
    inner: Builder,
}

impl RequestBuilder {
    pub fn new(method: http::method::Method, url: http::Uri) -> Self {
        // Enforce the need for a method and an url
        let builder = Builder::new().method(method).uri(url.clone());
        Self {
            url,
            inner: builder,
        }
    }

    /// Set the HTTP version for this request.
    pub fn version(self, version: Version) -> Self {
        Self {
            url: self.url,
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
            url: self.url,
            inner: self.inner.header(key, value),
        }
    }

    /// "Consumes" this builder, using the provided `body` to return a
    /// constructed `Request`.
    pub fn body<T>(self, body: T) -> http::Result<Request<T>> {
        Ok(Request {
            url: self.url,
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
