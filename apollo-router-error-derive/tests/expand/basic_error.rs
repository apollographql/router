use apollo_router_error_derive::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum BasicError {
    #[error("Basic error occurred")]
    #[diagnostic(code(apollo_router::service::basic_error))]
    BasicError,
    
    #[error("Config error: {message}")]
    #[diagnostic(
        code(apollo_router::service::config_error),
        help("Check your configuration")
    )]
    ConfigError { message: String },
} 