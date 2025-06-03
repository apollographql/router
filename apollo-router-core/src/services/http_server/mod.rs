use bytes::Bytes;
use http_body::Body;
use http_body_util::combinators::BoxBody;
use services::context::Context;
use thiserror::Error;
use tower::util::BoxCloneService;
use tower::BoxError;

#[derive(Clone)]
pub struct Request {
    pub context: Context,
    pub body: BoxBody<Bytes, BoxError>,
}

type ResponseValue = serde_json::Value;

pub struct Response {
    pub context: Context,
    pub responses: BoxBody<Bytes, BoxError>,
}

#[derive(Debug, Error)]
enum Error {}

type HttpServerService = BoxCloneService<Request, Result<Response, Error>, BoxError>;
