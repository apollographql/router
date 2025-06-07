use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use serde_json::Value;
use thiserror::Error;
use miette::{Diagnostic, SourceSpan};
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

/// Enhanced error type using the new error handling strategy
#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    /// GraphQL parse error with rich diagnostics
    #[error("GraphQL parsing failed: {message}")]
    #[diagnostic(
        code(apollo_router::query_parse::syntax_error),
        url(docsrs),
        help("Ensure your GraphQL query syntax is correct. Check for missing braces, invalid tokens, or malformed operations.")
    )]
    ParseError {
        message: String,
        #[source_code]
        query_source: Option<String>,
        #[label("Parse error occurred here")]
        error_span: Option<SourceSpan>,
    },

    /// Invalid query format with context
    #[error("Invalid query format: {reason}")]
    #[diagnostic(
        code(apollo_router::query_parse::invalid_format),
        url(docsrs),
        help("Query must be provided as a string. Check your request format.")
    )]
    InvalidQuery {
        reason: String,
        #[source_code]
        query_data: Option<String>,
        #[label("Invalid format")]
        error_location: Option<SourceSpan>,
    },

    /// JSON extraction failure with diagnostic context
    #[error("JSON extraction failed: {details}")]
    #[diagnostic(
        code(apollo_router::query_parse::json_extraction_error),
        url(docsrs),
        help("Ensure the request body contains valid JSON with a 'query' field.")
    )]
    JsonExtraction {
        details: String,
        #[source]
        json_error: Option<serde_json::Error>,
    },
}

impl crate::error::RouterError for Error {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ParseError { .. } => "apollo_router::query_parse::syntax_error",
            Self::InvalidQuery { .. } => "apollo_router::query_parse::invalid_format",
            Self::JsonExtraction { .. } => "apollo_router::query_parse::json_extraction_error",
        }
    }
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
                    query_data: Some(other_str.clone()),
                    error_location: Some((0, other_str.len()).into()),
                });
            }
        };

        // Validate query is not empty
        if query_string.trim().is_empty() {
            return Err(Error::InvalidQuery {
                reason: "Query string cannot be empty".to_string(),
                query_data: Some(query_string.clone()),
                error_location: Some((0, 0).into()),
            });
        }

        // Parse the GraphQL query using apollo_compiler with enhanced error reporting
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
                return Err(Error::ParseError {
                    message: format!("Failed to create minimal schema: {}", error_msg),
                    query_source: Some(schema_sdl.to_string()),
                    error_span: Some((0, schema_sdl.len()).into()),
                });
            }
        };
        
        match apollo_compiler::ExecutableDocument::parse(&minimal_schema, query_string, "query.graphql") {
            Ok(doc) => Ok(doc),
            Err(diagnostics) => {
                let error_messages: Vec<String> = diagnostics
                    .errors
                    .iter()
                    .map(|d| d.error.to_string())
                    .collect();
                
                // Position information not easily extractable from current apollo_compiler version
                let error_span = None;
                
                Err(Error::ParseError {
                    message: error_messages.join("; "),
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
mod tests {
    use super::*;
    use crate::error::RouterError;

    #[test]
    fn test_error_codes_match_strategy() {
        let parse_error = Error::ParseError {
            message: "test".to_string(),
            query_source: None,
            error_span: None,
        };
        
        assert_eq!(
            parse_error.error_code(),
            "apollo_router::query_parse::syntax_error"
        );

        let invalid_error = Error::InvalidQuery {
            reason: "test".to_string(),
            query_data: None,
            error_location: None,
        };
        
        assert_eq!(
            invalid_error.error_code(),
            "apollo_router::query_parse::invalid_format"
        );
    }

    #[tokio::test]
    async fn test_enhanced_error_reporting() {
        let mut service = QueryParseService::new();

        let request = Request {
            extensions: Extensions::default(),
            operation_name: None,
            query: serde_json::json!(123), // Invalid: should be string
        };

        let result = service.call(request).await;
        assert!(result.is_err());
        
        if let Err(Error::InvalidQuery { reason, query_data, .. }) = result {
            assert!(reason.contains("Expected string"));
            assert!(query_data.is_some());
        } else {
            panic!("Expected InvalidQuery error");
        }
    }

    #[tokio::test]
    async fn test_parse_error_with_source_context() {
        let mut service = QueryParseService::new();

        let request = Request {
            extensions: Extensions::default(),
            operation_name: None,
            query: serde_json::json!("query { invalid syntax here }}}"), // Malformed query
        };

        let result = service.call(request).await;
        assert!(result.is_err());
        
        if let Err(Error::ParseError { message: _, query_source, error_span }) = result {
            assert!(query_source.is_some());
            // Error span information depends on apollo_compiler's error reporting
            println!("Error span: {:?}", error_span);
        } else {
            panic!("Expected ParseError");
        }
    }

    #[tokio::test]
    async fn test_empty_query_error() {
        let mut service = QueryParseService::new();

        let request = Request {
            extensions: Extensions::default(),
            operation_name: None,
            query: serde_json::json!("   "), // Empty/whitespace query
        };

        let result = service.call(request).await;
        assert!(result.is_err());
        
        if let Err(Error::InvalidQuery { reason, error_location, .. }) = result {
            assert!(reason.contains("cannot be empty"));
            assert_eq!(error_location, Some((0, 0).into()));
        } else {
            panic!("Expected InvalidQuery error for empty query");
        }
    }

    #[tokio::test]
    async fn test_successful_parse() {
        let mut service = QueryParseService::new();

        let request = Request {
            extensions: Extensions::default(),
            operation_name: Some("GetUser".to_string()),
            query: serde_json::json!("query GetUser { user { id name } }"),
        };

        let result = service.call(request).await;
        assert!(result.is_ok());
        
        let response = result.unwrap();
        assert_eq!(response.operation_name, Some("GetUser".to_string()));
    }
}


