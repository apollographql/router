use std::collections::HashMap;
use std::pin::Pin;

use apollo_federation::query_plan::QueryPlan;
use apollo_router_error::Error as RouterError;
use futures::Stream;
use serde_json::Value;
use tower::BoxError;

use crate::Extensions;
use crate::json::JsonValue;

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

#[derive(Debug, thiserror::Error, miette::Diagnostic, RouterError)]
pub enum Error {
    /// Query execution failed: {message}
    #[error("Query execution failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_EXECUTION_EXECUTION_FAILED),
        help("Check your query execution configuration and subgraph availability")
    )]
    ExecutionFailed {
        #[extension("executionMessage")]
        message: String,
    },

    /// Fetch operation failed: {message}
    #[error("Fetch operation failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_EXECUTION_FETCH_FAILED),
        help("Verify subgraph connectivity and response format")
    )]
    FetchFailed {
        #[extension("fetchMessage")]
        message: String,
    },

    /// Planning error: {message}
    #[error("Planning error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_EXECUTION_PLANNING_FAILED),
        help("Check your query plan generation and federation configuration")
    )]
    PlanningFailed {
        #[extension("planningMessage")]
        message: String,
    },
}
