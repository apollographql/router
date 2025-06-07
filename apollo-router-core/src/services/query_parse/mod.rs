use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use miette::SourceSpan;
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

/// Query parsing errors with comprehensive diagnostics and GraphQL extension support
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// GraphQL parse error with rich diagnostics
    #[error("GraphQL parsing failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR),
        help("Ensure your GraphQL query syntax is correct. Check for missing braces, invalid tokens, or malformed operations.")
    )]
    ParseError {
        #[extension("parseMessage")]
        message: String,
        #[extension("hasSourceCode")]
        has_source_code: bool,
        #[source_code]
        query_source: Option<String>,
        #[label("Parse error occurred here")]
        error_span: Option<SourceSpan>,
    },

    /// Invalid query format with context
    #[error("Invalid query format: {reason}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_INVALID_FORMAT),
        help("Query must be provided as a string. Check your request format.")
    )]
    InvalidQuery {
        #[extension("invalidReason")]
        reason: String,
        #[extension("actualType")]
        actual_type: String,
        #[extension("isEmpty")]
        is_empty: bool,
        #[source_code]
        query_data: Option<String>,
        #[label("Invalid format")]
        error_location: Option<SourceSpan>,
    },

    /// JSON extraction failure with diagnostic context
    #[error("JSON extraction failed: {details}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_JSON_EXTRACTION_ERROR),
        help("Ensure the request body contains valid JSON with a 'query' field.")
    )]
    JsonExtraction {
        #[extension("extractionDetails")]
        details: String,
        #[source]
        json_error: Option<serde_json::Error>,
    },

    /// Schema validation failure during parsing
    #[error("Schema validation failed: {reason}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_SCHEMA_VALIDATION_ERROR),
        help("Check that your query is valid against the provided schema.")
    )]
    SchemaValidation {
        #[extension("validationReason")]
        reason: String,
        #[extension("operationType")]
        operation_type: Option<String>,
        #[source_code]
        schema_source: Option<String>,
        #[label("Schema validation error")]
        error_span: Option<SourceSpan>,
    },
}

/// Query parsing service that transforms JSON query values into parsed ExecutableDocuments
#[derive(Clone, Debug)]
pub struct QueryParseService;

impl QueryParseService {
    pub fn new() -> Self {
        Self
    }

    /// Parse a GraphQL query string into an ExecutableDocument with enhanced error reporting
    fn parse_query(&self, query_value: &Value) -> Result<ExecutableDocument, Error> {
        // Extract query string from JSON value with enhanced error context
        let query_string = match query_value {
            Value::String(s) => s,
            other => {
                let other_str = other.to_string();
                return Err(Error::InvalidQuery {
                    reason: format!("Expected string, found {}", other.type_name()),
                    actual_type: other.type_name().to_string(),
                    is_empty: false,
                    query_data: Some(other_str.clone()),
                    error_location: Some((0, other_str.len()).into()),
                });
            }
        };

        // Validate query is not empty
        if query_string.trim().is_empty() {
            return Err(Error::InvalidQuery {
                reason: "Query string cannot be empty".to_string(),
                actual_type: "string".to_string(),
                is_empty: true,
                query_data: Some(query_string.clone()),
                error_location: Some((0, 0).into()),
            });
        }

        // Create minimal schema for parsing validation
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
                return Err(Error::SchemaValidation {
                    reason: format!("Failed to create minimal schema: {}", error_msg),
                    operation_type: None,
                    schema_source: Some(schema_sdl.to_string()),
                    error_span: Some((0, schema_sdl.len()).into()),
                });
            }
        };
        
        // Parse the GraphQL query using apollo_compiler with enhanced error reporting
        match apollo_compiler::ExecutableDocument::parse(&minimal_schema, query_string, "query.graphql") {
            Ok(doc) => Ok(doc),
            Err(diagnostics) => {
                let error_messages: Vec<String> = diagnostics
                    .errors
                    .iter()
                    .map(|d| d.error.to_string())
                    .collect();
                
                // TODO: Extract position information from apollo_compiler diagnostics when available
                let error_span = None;
                
                Err(Error::ParseError {
                    message: error_messages.join("; "),
                    has_source_code: true,
                    query_source: Some(query_string.clone()),
                    error_span,
                })
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
        let query = req.query.clone();
        let operation_name = req.operation_name.clone();
        let extensions = req.extensions;
        let service = self.clone();

        Box::pin(async move {
            let parsed_query = service.parse_query(&query)?;

            Ok(Response {
                extensions,
                operation_name,
                query: parsed_query,
            })
        })
    }
}

trait ValueTypeName {
    fn type_name(&self) -> &'static str;
}

impl ValueTypeName for Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

#[cfg(test)]
mod tests;


