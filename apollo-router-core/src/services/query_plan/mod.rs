use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_federation::query_plan::QueryPlan;
use thiserror::Error;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,

    // TODO maybe wrap to make immutable
    pub query_plan: QueryPlan,
}

#[derive(Debug, Error)]
pub enum Error {
    /// Query planning failed: {0}
    #[error("Query planning failed: {0}")]
    PlanningError(String),

    /// Schema validation failed: {0}
    #[error("Schema validation failed: {0}")]
    SchemaValidation(String),

    /// Federation error: {0}
    #[error("Federation error: {0}")]
    FederationError(String),
}

#[cfg_attr(test, mry::mry)]
#[allow(async_fn_in_trait)]
pub trait QueryPlanning {
    async fn call(&self, req: Request) -> Result<Response, Error>;
}
