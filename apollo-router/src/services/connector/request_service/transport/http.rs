//! HTTP transport for Apollo Connectors
use crate::plugins::connectors::plugin::debug::ConnectorDebugHttpRequest;
use crate::services::router::body::RouterBody;
use http::Version;
use sha2::Digest;
use sha2::Sha256;

/// Request to an HTTP transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct HttpRequest {
    pub(crate) inner: http::Request<RouterBody>,
    pub(crate) raw_body: String,
    pub(crate) debug: Option<ConnectorDebugHttpRequest>,
}

impl HttpRequest {
    /// Create a unique hash to identify this HTTP request
    pub(crate) fn to_sha256(&self) -> String {
        let mut hasher = Sha256::new();
        // Http method
        hasher.update(self.inner.method().as_str().as_bytes());

        // HTTP Version
        let version = match self.inner.version() {
            Version::HTTP_09 => "HTTP/0.9",
            Version::HTTP_10 => "HTTP/1.0",
            Version::HTTP_11 => "HTTP/1.1",
            Version::HTTP_2 => "HTTP/2.0",
            Version::HTTP_3 => "HTTP/3.0",
            _ => "unknown",
        };
        hasher.update(version.as_bytes());

        // URI information
        let uri = self.inner.uri();
        if let Some(scheme) = uri.scheme() {
            hasher.update(scheme.as_str().as_bytes());
        }
        if let Some(authority) = uri.authority() {
            hasher.update(authority.as_str().as_bytes());
        }
        if let Some(query) = uri.query() {
            hasher.update(query.as_bytes());
        }

        // Headers... This assumes headers are in the same order
        for (name, value) in self.inner.headers() {
            hasher.update(name.as_str().as_bytes());
            hasher.update(value.to_str().unwrap_or("ERROR").as_bytes());
        }

        // Body
        // TODO: This is where the "raw_body" is needed... I can't use self.inner.body without draining the bytes... maybe theres a better way? I hope so!
        hasher.update(self.raw_body.as_str().as_bytes());

        // Return a hash!
        hex::encode(hasher.finalize())
    }
}

/// Response from an HTTP transport
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct HttpResponse {
    /// The response parts - the body is consumed by applying the JSON mapping
    pub(crate) inner: http::response::Parts,
}
