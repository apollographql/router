use crate::Extensions;
use crate::json::JsonValue;
use apollo_federation::query_plan::QueryPlan;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

pub struct Request {
    pub extensions: Extensions,
    pub body: JsonValue,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}
