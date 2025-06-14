use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use tower::BoxError;

use crate::Extensions;

pub struct Request {
    pub extensions: Extensions,
    pub body: Bytes,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Bytes, BoxError>> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("extensions", &self.extensions)
            .field("responses", &"<stream>")
            .finish()
    }
}
