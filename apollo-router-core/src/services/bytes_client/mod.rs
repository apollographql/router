use crate::Extensions;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tower::Service;

#[derive(Clone, Debug)]
pub struct Request {
    pub extensions: Extensions,
    pub body: Bytes,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Bytes> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
enum Error {}
