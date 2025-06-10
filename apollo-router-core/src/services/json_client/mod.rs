use crate::Extensions;
use crate::json::JsonValue;
use futures::Stream;
use std::pin::Pin;
use tower::BoxError;

pub struct Request {
    pub extensions: Extensions,
    pub body: JsonValue,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<JsonValue, BoxError>> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}
