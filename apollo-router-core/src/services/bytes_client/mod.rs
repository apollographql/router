use crate::Extensions;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use tower::BoxError;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub body: Bytes,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Bytes, BoxError>> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}
