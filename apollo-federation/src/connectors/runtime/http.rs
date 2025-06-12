//! HTTP transport for Apollo Connectors

use crate::connectors::runtime::debug::ConnectorDebugHttpRequest;

/// Request to an HTTP transport
#[derive(Debug)]
// #[non_exhaustive]
pub struct HttpRequest {
    pub inner: http::Request<String>,
    pub debug: Option<Box<ConnectorDebugHttpRequest>>,
}

/// Response from an HTTP transport
#[derive(Debug)]
// #[non_exhaustive]
pub struct HttpResponse {
    /// The response parts - the body is consumed by applying the JSON mapping
    pub inner: http::response::Parts,
}

/// Request to an underlying transport
#[derive(Debug)]
// #[non_exhaustive]
pub enum TransportRequest {
    /// A request to an HTTP transport
    Http(HttpRequest),
}

/// Response from an underlying transport
#[derive(Debug)]
// #[non_exhaustive]
pub enum TransportResponse {
    /// A response from an HTTP transport
    Http(HttpResponse),
}

impl From<HttpRequest> for TransportRequest {
    fn from(value: HttpRequest) -> Self {
        Self::Http(value)
    }
}

impl From<HttpResponse> for TransportResponse {
    fn from(value: HttpResponse) -> Self {
        Self::Http(value)
    }
}
