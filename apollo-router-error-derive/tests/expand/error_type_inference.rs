use apollo_router_error_derive::Error;

#[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
pub enum InferenceError {
    #[error("Syntax error")]
    #[diagnostic(code(apollo_router::service::syntax_error))]
    SyntaxError,
    
    #[error("Config error")]
    #[diagnostic(code(apollo_router::service::config_error))]
    ConfigError,
    
    #[error("Timeout error")]
    #[diagnostic(code(apollo_router::service::timeout))]
    TimeoutError,
    
    #[error("Network error")]
    #[diagnostic(code(apollo_router::service::network_error))]
    NetworkError,
    
    #[error("Conversion error")]
    #[diagnostic(code(apollo_router::service::conversion_error))]
    ConversionError,
    
    #[error("JSON error")]
    #[diagnostic(code(apollo_router::service::json_error))]
    JsonError,
} 