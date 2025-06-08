use crate::Extensions;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_compiler::validation::{Valid, WithErrors};
use apollo_router_error::Error as RouterError;
use serde::{Deserialize, Serialize};
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

/// Serializable error detail for individual parsing errors in GraphQL extensions
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error, miette::Diagnostic, RouterError)]
pub enum ParseErrorDetail {
    /// Syntax error in GraphQL query: {message}
    #[error("Syntax error in GraphQL query: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_SYNTAX_ERROR),
        help("Check your GraphQL query syntax for errors")
    )]
    SyntaxError {
        #[extension("syntaxMessage")]
        message: String,
        #[extension("line")]
        line: Option<u32>,
        #[extension("column")]
        column: Option<u32>,
    },

    /// Validation error: {message}
    #[error("Validation error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_VALIDATION_ERROR),
        help("Ensure your GraphQL query is valid according to the schema")
    )]
    ValidationError {
        #[extension("validationMessage")]
        message: String,
        #[extension("line")]
        line: Option<u32>,
        #[extension("column")]
        column: Option<u32>,
    },

    /// Unknown field error: {field_name} on type {type_name}
    #[error("Unknown field '{field_name}' on type '{type_name}'")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_UNKNOWN_FIELD),
        help("Check that the field exists on the specified type in your schema")
    )]
    UnknownField {
        #[extension("fieldName")]
        field_name: String,
        #[extension("typeName")]
        type_name: String,
        #[extension("line")]
        line: Option<u32>,
        #[extension("column")]
        column: Option<u32>,
    },

    /// Type mismatch error: {message}
    #[error("Type mismatch: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_TYPE_MISMATCH),
        help("Ensure field types match the schema definition")
    )]
    TypeMismatch {
        #[extension("typeMessage")]
        message: String,
        #[extension("line")]
        line: Option<u32>,
        #[extension("column")]
        column: Option<u32>,
    },

    /// Other parsing error: {message}
    #[error("Other parsing error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_OTHER_ERROR),
        help("Review the error message for specific details")
    )]
    Other {
        #[extension("errorMessage")]
        message: String,
        #[extension("line")]
        line: Option<u32>,
        #[extension("column")]
        column: Option<u32>,
    },
}

impl From<apollo_compiler::diagnostic::Diagnostic<'_, apollo_compiler::validation::DiagnosticData>> for ParseErrorDetail {
    fn from(diagnostic: apollo_compiler::diagnostic::Diagnostic<'_, apollo_compiler::validation::DiagnosticData>) -> Self {
        // Use to_string() to get the error message from Diagnostic
        let message = diagnostic.to_string();
        
        // Extract location information using line_column_range()
        let (line, column) = if let Some(range) = diagnostic.line_column_range() {
            (
                Some(range.start.line as u32),
                Some(range.start.column as u32),
            )
        } else {
            (None, None)
        };

        // Try to categorize the error based on the message content
        if message.contains("syntax") || message.contains("expected") || message.contains("unexpected") {
            Self::SyntaxError { message, line, column }
        } else if message.contains("Unknown field") {
            // Try to extract field and type names from error message
            // Format is typically "Unknown field 'fieldName' on type 'TypeName'"
            let field_name = extract_quoted_text(&message, 0).unwrap_or_else(|| "unknown".to_string());
            let type_name = extract_quoted_text(&message, 1).unwrap_or_else(|| "unknown".to_string());
            Self::UnknownField { field_name, type_name, line, column }
        } else if message.contains("type") && (message.contains("mismatch") || message.contains("expected")) {
            Self::TypeMismatch { message, line, column }
        } else {
            // Default to validation error for most apollo_compiler diagnostics
            Self::ValidationError { message, line, column }
        }
    }
}

/// Extract text within single quotes from a string, returning the nth occurrence
fn extract_quoted_text(text: &str, occurrence: usize) -> Option<String> {
    let mut count = 0;
    let mut start = None;
    let mut chars = text.char_indices();
    
    while let Some((i, ch)) = chars.next() {
        if ch == '\'' {
            if start.is_none() {
                start = Some(i + 1);
            } else if let Some(start_pos) = start {
                if count == occurrence {
                    return Some(text[start_pos..i].to_string());
                }
                count += 1;
                start = None;
            }
        }
    }
    None
}

#[derive(Debug, thiserror::Error, miette::Diagnostic, RouterError, serde::Serialize)]
pub enum Error {
    /// Query parsing failed: {message}
    #[error("Query parsing failed: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_PARSING_FAILED),
        help("Check your GraphQL query syntax and schema compatibility")
    )]
    ParsingFailed {
        #[extension("parsingMessage")]
        message: String,
    },

    /// Multiple query parsing errors occurred
    #[error("Multiple query parsing errors occurred: {count} errors")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_MULTIPLE_PARSING_ERRORS),
        help("Review all parsing errors and fix the underlying issues")
    )]
    MultipleParsingErrors {
        #[extension("errorCount")]
        count: usize,
        #[extension("parsingErrors")]
        errors: Vec<ParseErrorDetail>,
    },

    /// Schema error: {message}
    #[error("Schema error: {message}")]
    #[diagnostic(
        code(APOLLO_ROUTER_QUERY_PARSE_SCHEMA_ERROR),
        help("Check your GraphQL schema configuration")
    )]
    SchemaError {
        #[extension("schemaMessage")]
        message: String,
    },
}

impl From<WithErrors<ExecutableDocument>> for Error {
    fn from(with_errors: WithErrors<ExecutableDocument>) -> Self {
        let errors: Vec<ParseErrorDetail> = with_errors.errors
            .iter()
            .map(|diagnostic| ParseErrorDetail::from(diagnostic))
            .collect();

        if errors.len() == 1 {
            // Single error case
            Self::ParsingFailed {
                message: errors[0].to_string(),
            }
        } else {
            // Multiple errors case
            Self::MultipleParsingErrors {
                count: errors.len(),
                errors,
            }
        }
    }
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

    /// Parse a GraphQL query string into a Valid<ExecutableDocument>
    /// 
    /// This method uses apollo_compiler's parse_and_validate and converts any validation
    /// errors into our structured error types.
    fn parse_query(&self, query_string: &str) -> Result<Valid<ExecutableDocument>, Error> {
        // Parse and validate the GraphQL query using apollo_compiler
        ExecutableDocument::parse_and_validate(&self.schema, query_string, "query.graphql")
            .map_err(Into::into)
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
            // Parse the query, returning Valid<ExecutableDocument> or Error
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
