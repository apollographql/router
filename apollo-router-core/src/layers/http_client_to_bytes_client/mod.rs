//! # HTTP Client to Bytes Client Layer
//!
//! The `HttpClientToBytesClientLayer` transforms HTTP client requests into bytes client requests
//! in the Apollo Router Core client-side pipeline. This layer is responsible for serializing
//! HTTP requests into bytes format and deserializing bytes responses back to HTTP format.
//!
//! ## Purpose
//!
//! - **HTTP Request Serialization**: Converts HTTP requests into bytes representation
//! - **Request Type Transformation**: Converts `HttpClientRequest` to `BytesClientRequest`
//! - **Protocol Abstraction**: Abstracts HTTP protocol details into raw bytes
//! - **Response Reconstruction**: Rebuilds HTTP responses from bytes data
//! - **Error Handling**: Provides structured error reporting for serialization failures
//!
//! ## Usage
//!
//! The layer is typically used in client-side pipelines for HTTP protocol abstraction:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (json_client, _handle) = tower_test::mock::spawn();
//! let client = ServiceBuilder::new()
//!     .http_client_to_bytes_client()  // Serialize HTTP to bytes
//!     .bytes_client_to_json_client()  // Further transform to JSON
//!     .service(json_client);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! HTTP Client Request
//!     ↓ Serialize HTTP request (method, URI, headers, body) to bytes
//!     ↓ Create default Extensions
//! Bytes Client Request → Inner Service
//!     ↓ Bytes Client Response
//!     ↓ Reconstruct HTTP response from bytes (placeholder)
//!     ↓ Set default headers and status
//! HTTP Client Response
//! ```
//!
//! ## Current Implementation Status
//!
//! **Note**: This layer currently contains placeholder implementation:
//! - **Request Serialization**: Basic serialization of HTTP request line only
//! - **Response Reconstruction**: Creates simple HTTP 200 responses with JSON content-type
//! - **Future Enhancement**: Full HTTP request/response serialization planned
//!
//! ## Extensions Handling
//!
//! This layer uses default Extensions:
//! - Creates new default Extensions for the bytes client request
//! - Does not currently preserve HTTP request Extensions
//! - **Future Enhancement**: Proper Extensions hierarchy management planned
//!
//! ## Error Handling
//!
//! The layer can produce `HttpClientToBytesClientError` in these situations:
//! - **Request Serialization**: When HTTP request cannot be serialized to bytes
//! - **Response Builder**: When HTTP response construction from bytes fails
//! - **Context Information**: Provides serialization and response building context
//!
//! ## Performance Considerations
//!
//! - **Placeholder Efficiency**: Current implementation is lightweight due to placeholders
//! - **Future Complexity**: Full implementation will require more processing overhead
//! - **Memory Usage**: Will need to serialize entire HTTP requests/responses
//! - **Protocol Overhead**: Adds serialization overhead for HTTP abstraction

use crate::services::bytes_client::{
    Request as BytesClientRequest, Response as BytesClientResponse,
};
use crate::services::http_client::{Request as HttpClientRequest, Response as HttpClientResponse};
use bytes::Bytes;
use http_body_util::BodyExt;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP request serialization failed
    #[error("HTTP request serialization failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_CLIENT_TO_BYTES_CLIENT_REQUEST_SERIALIZATION_ERROR),
        help("Check that the HTTP request can be properly serialized to bytes")
    )]
    RequestSerialization {
        #[extension("context")]
        context: String,
        #[extension("details")]
        details: String,
    },

    /// HTTP response building failed
    #[error("HTTP response building failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_CLIENT_TO_BYTES_CLIENT_RESPONSE_BUILD_ERROR),
        help("Check that the HTTP response can be properly constructed from bytes")
    )]
    ResponseBuilder {
        #[source]
        http_error: http::Error,
        #[extension("context")]
        context: String,
    },
}

/// A Tower layer that transforms HTTP client requests into bytes client requests.
///
/// This layer sits in client-side pipelines and provides HTTP protocol abstraction by:
/// - Serializing HTTP requests (method, URI, headers, body) into bytes format
/// - Converting HTTP responses back from bytes representation
/// - Currently contains placeholder implementation for future full HTTP serialization
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::http_client_to_bytes_client::HttpClientToBytesClientLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (bytes_client, _handle) = tower_test::mock::spawn();
/// let layer = HttpClientToBytesClientLayer;
/// let service = layer.layer(bytes_client);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct HttpClientToBytesClientLayer;

impl<S> Layer<S> for HttpClientToBytesClientLayer {
    type Service = HttpClientToBytesClientService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpClientToBytesClientService { inner: service }
    }
}

/// The service implementation that performs HTTP client to bytes client transformation.
///
/// This service:
/// 1. Serializes the HTTP request to bytes (currently placeholder implementation)
/// 2. Creates a bytes client request with default Extensions
/// 3. Calls the inner bytes client service
/// 4. Reconstructs an HTTP response from the bytes response (placeholder)
/// 5. Returns a standard HTTP 200 response with JSON content-type
///
/// **Note**: Current implementation uses placeholders for full HTTP serialization.
#[derive(Clone, Debug)]
pub struct HttpClientToBytesClientService<S> {
    inner: S,
}

impl<S> Service<HttpClientRequest> for HttpClientToBytesClientService<S>
where
    S: Service<BytesClientRequest, Response = BytesClientResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = HttpClientResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpClientRequest) -> Self::Future {
        // Convert HTTP request to bytes
        // For now, serialize as empty bytes - in real implementation,
        // would serialize the HTTP request headers, method, URI, and body
        let request_bytes = serialize_http_request(&req);

        let bytes_client_req = BytesClientRequest {
            extensions: crate::Extensions::default(),
            body: request_bytes,
        };

        let future = self.inner.call(bytes_client_req);

        Box::pin(async move {
            // Await the inner service call
            let _bytes_resp = future.await.map_err(Into::into)?;

            // Transform BytesClientResponse back to HttpClientResponse
            // For now, create a simple HTTP response - in real implementation,
            // would parse the bytes stream back into proper HTTP responses
            use http_body_util::BodyExt;
            let body = http_body_util::Full::new(bytes::Bytes::from("{}"))
                .map_err(Into::into)
                .boxed_unsync();

            let http_resp = http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(body)
                .map_err(|http_error| Error::ResponseBuilder {
                    http_error,
                    context: "Building HTTP response from bytes in client layer".to_string(),
                })?;

            Ok(http_resp)
        })
    }
}

fn serialize_http_request(
    http_req: &http::Request<http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, BoxError>>,
) -> Bytes {
    // Placeholder implementation - in real scenario, would serialize
    // the HTTP request (method, URI, headers, body) into bytes
    let request_line = format!("{} {} HTTP/1.1\r\n", http_req.method(), http_req.uri());
    Bytes::from(request_line)
}

#[cfg(test)]
mod tests;
