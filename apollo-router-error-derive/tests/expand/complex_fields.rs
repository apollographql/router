use apollo_router_error_derive::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum ComplexError {
    #[error("Parse error: {reason}")]
    #[diagnostic(
        code(apollo_router::service::parse_error),
        help("Check syntax")
    )]
    ParseError {
        reason: String,
        line: u32,
        column: u32,
        #[source_code]
        source_text: Option<String>,
        #[label("Error here")]
        span: Option<String>,
    },
    
    #[error("Network error for {endpoint}")]
    #[diagnostic(code(apollo_router::service::network_error))]
    NetworkError {
        endpoint: String,
        #[source]
        io_error: String,
    },
    
    #[error("JSON error: {0}")]
    #[diagnostic(code(apollo_router::service::json_error))]
    JsonError(String),
} 