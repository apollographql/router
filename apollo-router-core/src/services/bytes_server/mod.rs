use crate::Extensions;
use apollo_federation::query_plan::QueryPlan;
use bytes::Bytes;
use futures::Stream;
use std::fmt::Debug;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;
use tower::util::BoxCloneService;

#[derive(Debug)]
pub struct Request {
    pub extensions: Extensions,
    pub body: Bytes,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Bytes> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}

impl Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Response")
    }
}

#[derive(Debug, Error)]
enum Error {}
