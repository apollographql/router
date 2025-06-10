use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use std::convert::Infallible;

pub type Request = http::Request<UnsyncBoxBody<Bytes, Infallible>>;
pub type Response = http::Response<UnsyncBoxBody<Bytes, Infallible>>;

/// Error types for HTTP client services
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP request failed
    #[error("HTTP request failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_HTTP_CLIENT_REQUEST_FAILED),
        help("Check network connectivity and target service availability")
    )]
    RequestFailed {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        #[extension("url")]
        url: String,
        #[extension("method")]
        method: String,
    },

    /// Invalid request construction
    #[error("Invalid HTTP request construction")]
    #[diagnostic(
        code(APOLLO_ROUTER_HTTP_CLIENT_INVALID_REQUEST),
        help("Ensure the HTTP request is properly formed")
    )]
    InvalidRequest {
        #[extension("details")]
        details: String,
    },

    /// Response processing failed
    #[error("HTTP response processing failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_HTTP_CLIENT_RESPONSE_PROCESSING_FAILED),
        help("Check if the response format is as expected")
    )]
    ResponseProcessingFailed {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        #[extension("context")]
        context: String,
    },
}

pub mod reqwest;

#[cfg(test)]
mod tests;
