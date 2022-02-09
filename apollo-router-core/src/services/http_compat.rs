//! wrapper typpes for Request and Response from the http crate to improve their usability

use std::ops::{Deref, DerefMut};

#[derive(Debug, Default)]
pub struct Request<T> {
    pub inner: http::Request<T>,
}

impl<T> Request<T> {
    pub fn set_url(&mut self, url: url::Url) -> Result<(), http::Error> {
        *self.inner.uri_mut() = http::Uri::try_from(url.as_str())?;
        Ok(())
    }

    pub fn into_parts(self) -> (http::request::Parts, T) {
        self.inner.into_parts()
    }

    pub fn map<F, U>(self, f: F) -> Request<U>
    where
        F: FnOnce(T) -> U,
    {
        self.inner.map(f).into()
    }
}

impl<T> Deref for Request<T> {
    type Target = http::Request<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
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
    fn from(request: Request<T>) -> Self {
        request.inner
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
