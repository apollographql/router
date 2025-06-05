use crate::services::JsonValue;
use crate::services::context::Context;
use apollo_federation::query_plan::QueryPlan;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

#[derive(Clone)]
pub struct Request {
    pub context: Context,
    pub body: JsonValue,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub context: Context,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
enum Error {}

type JsonServerService = BoxCloneService<Request, Response, BoxError>;
