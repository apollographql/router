use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use std::convert::Infallible;

pub type Request = http::Request<UnsyncBoxBody<Bytes, Infallible>>;
pub type Response = http::Response<UnsyncBoxBody<Bytes, Infallible>>;
