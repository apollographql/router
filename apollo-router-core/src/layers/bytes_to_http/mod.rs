//! # Bytes to HTTP Layer
//!
//! The `BytesToHttpLayer` transforms bytes client requests into HTTP client requests in the 
//! Apollo Router Core client-side pipeline. This layer is responsible for wrapping bytes
//! data in HTTP requests for transmission to HTTP services and converting HTTP responses
//! back to bytes format.
//!
//! ## Purpose
//!
//! - **HTTP Request Construction**: Creates HTTP requests from bytes data
//! - **Request Type Transformation**: Converts `BytesRequest` to `HttpRequest` for HTTP clients
//! - **Extensions Management**: Properly handles Extensions hierarchy using `extend()` pattern
//! - **Response Processing**: Converts HTTP responses back to bytes streams
//! - **HTTP Protocol Handling**: Manages HTTP-specific headers, methods, and status codes
//!
//! ## Usage
//!
//! The layer is typically used in client-side pipelines as the final transformation before HTTP transmission:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (http_client, _handle) = tower_test::mock::spawn();
//! let client = ServiceBuilder::new()
//!     .json_to_bytes()      // Serialize JSON to bytes
//!     .bytes_to_http()      // Wrap in HTTP requests
//!     .service(http_client);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! Bytes Request (client-side)
//!     ↓ Create HTTP POST request with bytes body
//!     ↓ Set content-type: application/json
//!     ↓ Convert Extensions to http::Extensions
//! HTTP Request → Inner Service
//!     ↓ HTTP Response
//!     ↓ Extract body as bytes stream
//!     ↓ Return original Extensions
//! Bytes Response
//! ```
//!
//! ## Extensions Handling
//!
//! This layer follows the standard Extensions pattern:
//! - Converts original Extensions to `http::Extensions` for the inner HTTP service
//! - HTTP service operates with standard `http::Extensions`
//! - Response returns the **original** Extensions from the bytes request
//! - Maintains Extensions compatibility across HTTP boundaries
//!
//! ## HTTP Request Configuration
//!
//! The layer creates HTTP requests with these defaults:
//! - **Method**: POST (suitable for GraphQL requests)
//! - **URI**: `/` (root path)
//! - **Content-Type**: `application/json`
//! - **Body**: Request bytes as HTTP body
//!
//! ## Error Handling
//!
//! The layer can produce `BytesToHttpError` in these situations:
//! - **HTTP Request Building**: When HTTP request construction fails
//! - **Request Context**: Provides context information for debugging
//!
//! ## Performance Considerations
//!
//! - **Service Cloning**: Requires service cloning for async HTTP body collection
//! - **Body Collection**: Collects entire HTTP response body into memory
//! - **Extensions Conversion**: Efficient conversion between Extensions types
//! - **HTTP Overhead**: Adds HTTP protocol overhead to bytes transmission

use crate::services::bytes_client::{Request as BytesRequest, Response as BytesResponse};
use crate::services::http_client::{Request as HttpRequest, Response as HttpResponse};
use bytes::Bytes;
use http_body_util::{BodyExt, combinators::UnsyncBoxBody};
use std::convert::Infallible;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

// Type alias to match exactly what http_client uses
type HttpBody = UnsyncBoxBody<Bytes, Infallible>;

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP request building failed
    #[error("HTTP request building failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_TO_HTTP_REQUEST_BUILD_ERROR),
        help("Check that the HTTP request parameters are valid")
    )]
    HttpRequestBuilder {
        #[source]
        http_error: http::Error,
        #[extension("requestContext")]
        context: String,
    },
}

/// A Tower layer that transforms bytes client requests into HTTP client requests.
///
/// This layer sits at the final stage of client-side pipelines and is responsible for:
/// - Creating HTTP POST requests from bytes data
/// - Setting appropriate HTTP headers (content-type: application/json)
/// - Converting Extensions to http::Extensions for HTTP services
/// - Converting HTTP responses back to bytes streams
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::bytes_to_http::BytesToHttpLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (http_client, _handle) = tower_test::mock::spawn();
/// let layer = BytesToHttpLayer::new();
/// let service = layer.layer(http_client);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct BytesToHttpLayer;

impl BytesToHttpLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BytesToHttpLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for BytesToHttpLayer {
    type Service = BytesToHttpService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesToHttpService { inner: service }
    }
}

/// The service implementation that performs the bytes to HTTP transformation.
///
/// This service:
/// 1. Creates an HTTP POST request with the bytes as the body
/// 2. Sets content-type header to application/json
/// 3. Converts Extensions to http::Extensions for the inner service
/// 4. Calls the inner HTTP service
/// 5. Collects the HTTP response body back to bytes
/// 6. Returns the original Extensions in the bytes response
#[derive(Clone, Debug)]
pub struct BytesToHttpService<S> {
    inner: S,
}

impl<S> Service<BytesRequest> for BytesToHttpService<S>
where
    S: Service<HttpRequest, Response = HttpResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = BytesResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: BytesRequest) -> Self::Future {
        use std::mem;

        let inner = self.inner.clone();
        // Clone is required here because we need to handle async HTTP body collection
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Create HTTP request
            let mut http_req = Self::create_http_request(req.body)?;

            // Convert original Extensions directly to http::Extensions for downstream service
            let original_extensions = req.extensions;
            let http_extensions: http::Extensions = original_extensions.clone().into();
            *http_req.extensions_mut() = http_extensions;

            // Call the inner service
            let http_resp = inner.call(http_req).await.map_err(Into::into)?;

            // Convert HTTP response to bytes response
            let (_parts, body) = http_resp.into_parts();

            // Since body error type is Infallible, collection cannot fail
            let collected_bytes = body.collect().await.unwrap().to_bytes();
            let bytes_stream = futures::stream::once(async move { Ok(collected_bytes) });

            let bytes_resp = BytesResponse {
                extensions: original_extensions,
                responses: Box::pin(bytes_stream),
            };

            Ok(bytes_resp)
        })
    }
}

impl<S> BytesToHttpService<S> {
    /// Create HTTP request from bytes - helper to avoid lifetime issues
    fn create_http_request(body_bytes: Bytes) -> Result<HttpRequest, BoxError> {
        let full_body = http_body_util::Full::new(body_bytes);
        let body: HttpBody = UnsyncBoxBody::new(full_body);

        let http_req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(body)
            .map_err(|http_error| Error::HttpRequestBuilder {
                http_error,
                context: "Building HTTP request from bytes".to_string(),
            })?;

        Ok(http_req)
    }
}

#[cfg(test)]
mod tests;
