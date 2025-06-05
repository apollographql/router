use crate::services::context::Context;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

#[derive(Clone)]
pub struct Request {
    pub context: Context,
    pub body: Bytes,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Bytes> + Send>>;

pub struct Response {
    pub context: Context,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
enum Error {}

type BytesClientService = BoxCloneService<Request, Response, BoxError>;
