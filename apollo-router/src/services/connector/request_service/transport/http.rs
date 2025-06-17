//! HTTP transport for Apollo Connectors
use apollo_federation::connectors::ProblemLocation;
use apollo_federation::connectors::runtime::debug::ConnectorDebugHttpRequest;
use apollo_federation::connectors::runtime::mapping::Problem;

/// Request to an HTTP transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct HttpRequest {
    pub(crate) inner: http::Request<String>,
    pub(crate) debug: (
        Option<Box<ConnectorDebugHttpRequest>>,
        Vec<(ProblemLocation, Problem)>,
    ),
}

/// Response from an HTTP transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct HttpResponse {
    /// The response parts - the body is consumed by applying the JSON mapping
    pub(crate) inner: http::response::Parts,
}
