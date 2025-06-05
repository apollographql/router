use crate::json::JsonValue;
use crate::services::context::Context;
use apollo_federation::query_plan::QueryPlan;
use bytes::Bytes;
use futures::Stream;
use std::any::Any;
use std::collections::HashMap;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

pub struct Request {
    pub context: Context,
    // Services are cached by name in the FetchService.
    pub service_name: String,
    // This is opaque data identified by type ID when constructing the downstream service
    pub body: Box<dyn Any>,
    pub variables: HashMap<String, JsonValue>,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub context: Context,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
enum Error {}
