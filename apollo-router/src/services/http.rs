use hyper::Body;

use crate::Context;

pub(crate) mod service;

#[non_exhaustive]
pub struct HttpRequest {
    pub http_request: http::Request<Body>,
    pub context: Context,
}

#[non_exhaustive]
pub struct HttpResponse {
    pub http_response: http::Response<Body>,
    pub context: Context,
}
