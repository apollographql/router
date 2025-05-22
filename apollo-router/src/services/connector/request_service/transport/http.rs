//! HTTP transport for Apollo Connectors
use crate::plugins::connectors::plugin::debug::ConnectorDebugHttpRequest;

/// Request to an HTTP transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct HttpRequest {
    pub(crate) inner: http::Request<String>,
    pub(crate) debug: Option<ConnectorDebugHttpRequest>,
}

/// Response from an HTTP transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct HttpResponse {
    /// The response parts - the body is consumed by applying the JSON mapping
    pub(crate) inner: http::response::Parts,
}
