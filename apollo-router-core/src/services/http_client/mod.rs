use crate::services::context::Context;
use bytes::Bytes;
use http_body::Body;
use http_body_util::combinators::BoxBody;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

type HttpClientService = BoxCloneService<
    http::Request<BoxBody<Bytes, BoxError>>,
    http::Response<BoxBody<Bytes, BoxError>>,
    BoxError,
>;
