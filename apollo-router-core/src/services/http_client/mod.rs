use crate::Extensions;
use bytes::Bytes;
use http_body::Body;
use http_body_util::combinators::{BoxBody, UnsyncBoxBody};
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

pub type Request = http::Request<UnsyncBoxBody<Bytes, BoxError>>;
pub type Response = http::Response<UnsyncBoxBody<Bytes, BoxError>>;
