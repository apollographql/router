use crate::Extensions;
use crate::json::JsonValue;
use apollo_federation::query_plan::QueryPlan;
use futures::Stream;
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use thiserror::Error;
use tower::BoxError;

pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query_plan: QueryPlan,
    pub query_variables: HashMap<String, Value>,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<JsonValue, BoxError>> + Send>>;

pub struct Response {
    pub extensions: Extensions,
    pub responses: ResponseStream,
}

#[derive(Debug, Error)]
pub enum Error {
    /// Query execution failed: {0}
    #[error("Query execution failed: {0}")]
    ExecutionError(String),

    /// Fetch operation failed: {0}
    #[error("Fetch operation failed: {0}")]
    FetchError(String),

    /// Planning error: {0}
    #[error("Planning error: {0}")]
    PlanningError(String),
}


