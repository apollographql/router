#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::stream::StreamExt;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;

#[non_exhaustive]
pub struct Request {
    /// Original request to the Router.
    pub http_request: http::Request<hyper::Body>,

    /// Context for extension
    pub context: Context,
}

impl From<http::Request<hyper::Body>> for Request {
    fn from(http_request: http::Request<hyper::Body>) -> Self {
        Self {
            http_request,
            context: Context::new(),
        }
    }
}

assert_impl_all!(Response: Send);
#[non_exhaustive]
pub struct Response {
    pub response: http::Response<hyper::Body>,
    pub context: Context,
}

impl Response {
    pub async fn next_response(&mut self) -> Option<Result<Bytes, hyper::Error>> {
        self.response.body_mut().next().await
    }

    pub(crate) fn new_from_response(
        response: http::Response<hyper::Body>,
        context: Context,
    ) -> Self {
        Self { response, context }
    }

    pub fn map<F>(self, f: F) -> Response
    where
        F: FnOnce(hyper::Body) -> hyper::Body,
    {
        Response {
            context: self.context,
            response: self.response.map(f),
        }
    }

    pub fn map_stream<F>(self, f: F) -> Self
    where
        F: 'static + Send + FnMut(hyper::Body) -> hyper::Body,
    {
        self.map(move |stream| stream.map(f).boxed())
    }
}
