use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use thiserror::Error;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: Value,
}

pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: ExecutableDocument,
}

#[derive(Debug, Error)]
pub enum Error {
    /// GraphQL parse error: {0}
    #[error("GraphQL parse error: {0}")]
    ParseError(String),

    /// Invalid query format: {0}
    #[error("Invalid query format: {0}")]
    InvalidQuery(String),

    /// JSON extraction failed: {0}
    #[error("JSON extraction failed: {0}")]
    JsonExtraction(String),
}

#[cfg_attr(test, mry::mry)]
pub trait QueryParse {
    async fn call(&self, req: Request) -> Result<Response, Error>;
}
