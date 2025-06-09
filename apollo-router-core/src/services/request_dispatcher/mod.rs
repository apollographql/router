use crate::Extensions;
use crate::json::JsonValue;
use futures::Stream;
use std::any::Any;
use std::collections::HashMap;
use std::pin::Pin;

pub struct Request {
    pub extensions: Extensions,
    // Services are cached by name in the FetchService.
    pub service_name: String,
    // This is opaque data identified by type ID when constructing the downstream service
    pub body: Box<dyn Any>,
    pub variables: HashMap<String, JsonValue>,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}
