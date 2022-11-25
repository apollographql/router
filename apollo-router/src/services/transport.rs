#![allow(missing_docs)] // FIXME

use bytes::Bytes;
use futures::stream::StreamExt;
use static_assertions::assert_impl_all;
use tower::BoxError;

use crate::Context;

pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;
pub type Response = http::Response<hyper::Body>;
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
