use crate::Extensions;
use crate::json::JsonValue;
use futures::Stream;
use std::pin::Pin;

pub struct Request {
    pub extensions: Extensions,
    pub body: JsonValue,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}
