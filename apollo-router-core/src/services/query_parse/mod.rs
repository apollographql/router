use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_compiler::response::GraphQLError;
use apollo_router_error::Error as RouterError;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

#[derive(Clone)]
pub struct Request {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: String,
}

#[derive(Debug)]
pub struct Response {
    pub extensions: Extensions,
    pub operation_name: Option<String>,
    pub query: Valid<ExecutableDocument>,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic, RouterError)]
pub enum Error {
    /// Query parsing failed: {message}
    #[error("Query parsing failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_PARSING_FAILED),
        help("Check your GraphQL query syntax")
    )]
    ParseError {
        #[extension("parseMessage")]
        message: String,
        #[extension("errors")]
        errors: Vec<GraphQLError>,
    },

    /// Query validation failed: {message}
    #[error("Query validation failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_VALIDATION_FAILED),
        help("Ensure your GraphQL query is valid according to the schema")
    )]
    ValidationError {
        #[extension("validationMessage")]
        message: String,
        #[extension("errors")]
        errors: Vec<GraphQLError>,
    },
}

/// Query parsing service that transforms query strings into parsed ExecutableDocuments with validation
#[derive(Clone, Debug)]
pub struct QueryParseService {
    schema: Valid<Schema>,
}

impl QueryParseService {
    pub fn new(schema: Valid<Schema>) -> Self {
        Self { schema }
    }

    /// Parse and validate a GraphQL query string into a Valid<ExecutableDocument>
    ///
    /// This method uses apollo_compiler's parse_and_validate and categorizes
    /// errors based on their content as either parse or validation errors.
    fn parse_query(&self, query_string: &str) -> Result<Valid<ExecutableDocument>, Error> {
        match ExecutableDocument::parse_and_validate(&self.schema, query_string, "query.graphql") {
            Ok(valid_document) => Ok(valid_document),
            Err(with_errors) => {
                let errors: Vec<GraphQLError> = with_errors
                    .errors
                    .iter()
                    .map(|diagnostic| diagnostic.to_json())
                    .collect();
                
                // Categorize errors based on content
                let has_syntax_errors = with_errors.errors.iter().any(|diagnostic| {
                    let message = diagnostic.to_string();
                    message.contains("syntax") || 
                    message.contains("expected") || 
                    message.contains("unexpected") ||
                    message.contains("parse error") ||
                    message.contains("parsing failed")
                });
                
                let message = if errors.len() == 1 {
                    if has_syntax_errors {
                        format!("Parse error in GraphQL query")
                    } else {
                        format!("Validation error in GraphQL query")
                    }
                } else {
                    if has_syntax_errors {
                        format!("Multiple parse errors in GraphQL query ({} errors)", errors.len())
                    } else {
                        format!("Multiple validation errors in GraphQL query ({} errors)", errors.len())
                    }
                };
                
                if has_syntax_errors {
                    Err(Error::ParseError { message, errors })
                } else {
                    Err(Error::ValidationError { message, errors })
                }
            }
        }
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
        let query_string = req.query.clone();
        let operation_name = req.operation_name.clone();
        let extensions = req.extensions;
        let service = self.clone();

        Box::pin(async move {
            // Parse and validate the query, returning Valid<ExecutableDocument> or Error
            let parsed_query = service.parse_query(query_string.as_str())?;

            Ok(Response {
                extensions,
                operation_name,
                query: parsed_query,
            })
        })
    }
}

#[cfg(test)]
mod tests;
