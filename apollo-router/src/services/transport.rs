#![allow(missing_docs)] // FIXME

use tower::BoxError;

pub type Request = http::Request<hyper::Body>;
pub type Response = http::Response<hyper::Body>;
pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;
