#![allow(missing_docs)] // FIXME

use tower::BoxError;

pub type Request = crate::http_ext::Request<hyper::Body>;
pub type Response = crate::http_ext::Response<hyper::Body>;
pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;
pub type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub type ServiceResult = Result<Response, BoxError>;
