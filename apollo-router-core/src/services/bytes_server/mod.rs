use crate::services::context::Context;
use apollo_federation::query_plan::QueryPlan;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tower::util::BoxCloneService;
use tower::BoxError;

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

type BytesServerService = BoxCloneService<Request, Result<Response, Error>, BoxError>;
