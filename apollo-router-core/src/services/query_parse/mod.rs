use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use thiserror::Error;
use tower::Service;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

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

/// Query parsing service that transforms JSON query values into parsed ExecutableDocuments
#[derive(Clone, Debug)]
pub struct QueryParseService;

impl QueryParseService {
    pub fn new() -> Self {
        Self
    }

    /// Parse a GraphQL query string into an ExecutableDocument
    fn parse_query(&self, query_value: &Value) -> Result<ExecutableDocument, Error> {
        // Extract query string from JSON value
        let query_string = match query_value {
            Value::String(s) => s,
            _ => return Err(Error::InvalidQuery("Query must be a string".to_string())),
        };

        // Parse the GraphQL query using apollo_compiler
        // Create a minimal schema for parsing (syntax validation only)
        let schema_sdl = r#"
            type Query {
                user: User
            }
            
            type User {
                id: ID!
                name: String
            }
            
            type Mutation {
                createUser(input: UserInput!): User
            }
            
            input UserInput {
                name: String!
            }
        "#;
        
        let minimal_schema = match apollo_compiler::Schema::parse_and_validate(schema_sdl, "schema.graphql") {
            Ok(schema) => schema,
            Err(diagnostics) => {
                let error_msg = diagnostics
                    .errors
                    .iter()
                    .map(|d| d.error.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(Error::ParseError(format!("Failed to create minimal schema: {}", error_msg)));
            }
        };
        
        match apollo_compiler::ExecutableDocument::parse(&minimal_schema, query_string, "query.graphql") {
            Ok(doc) => Ok(doc),
            Err(diagnostics) => {
                let error_msg = diagnostics
                    .errors
                    .iter()
                    .map(|d| d.error.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                Err(Error::ParseError(error_msg))
            }
        }
    }
}

impl Default for QueryParseService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service<Request> for QueryParseService {
    type Response = Response;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            let parsed_query = service.parse_query(&req.query)?;
            
            Ok(Response {
                extensions: req.extensions,
                operation_name: req.operation_name,
                query: parsed_query,
            })
        })
    }
}

#[cfg(test)]
mod tests;
