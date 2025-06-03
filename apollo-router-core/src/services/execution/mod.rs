use crate::services::context::Context;
use apollo_federation::query_plan::QueryPlan;
use futures::Stream;
use serde_json::Value;
use services::JsonValue;
use std::collections::HashMap;
use std::pin::Pin;
use thiserror::Error;
use tower::util::BoxCloneService;
use tower::BoxError;

pub struct Request {
    pub context: Context,
    pub operation_name: Option<String>,
    pub query_plan: QueryPlan,
    pub query_variables: HashMap<String, Value>,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = JsonValue> + Send>>;

pub struct Response {
    pub context: Context,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
enum Error {}

type QueryPlannerService = BoxCloneService<Request, Result<Response, Error>, BoxError>;
