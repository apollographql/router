use crate::Extensions;
use crate::json::JsonValue;
use apollo_federation::query_plan::QueryPlan;
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub body: JsonValue,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query_plan: QueryPlan,
    pub query_variables: HashMap<String, Value>,
}

#[derive(Debug, Error)]
pub enum Error {
    /// Query planning failed: {0}
    #[error("Query planning failed: {0}")]
    QueryPlanning(#[from] crate::services::query_plan::Error),

    /// JSON extraction failed: {0}
    #[error("JSON extraction failed: {0}")]
    JsonExtraction(String),

    /// Variable extraction failed: {0}
    #[error("Variable extraction failed: {0}")]
    VariableExtraction(String),
}

#[cfg_attr(test, mry::mry)]
#[allow(async_fn_in_trait)]
pub trait QueryPreparation {
    async fn call(&self, req: Request) -> Result<Response, Error>;
}
