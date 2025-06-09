//! # HTTP to Bytes Layer
//!
//! The `HttpToBytesLayer` transforms HTTP requests into bytes requests in the Apollo Router Core 
//! request pipeline. This layer is responsible for extracting the body from incoming HTTP requests
//! and converting them into a bytes representation that can be processed by downstream services.
//!
//! ## Purpose
//!
//! - **HTTP Body Extraction**: Collects the entire HTTP request body into bytes
//! - **Request Type Transformation**: Converts `HttpRequest` to `BytesRequest`
//! - **Extensions Management**: Properly handles Extensions hierarchy using `extend()` pattern
//! - **Error Handling**: Provides structured error reporting for HTTP response building failures
//!
//! ## Usage
//!
//! The layer is typically used early in the server-side request pipeline:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (inner_service, _handle) = tower_test::mock::spawn();
//! let service = ServiceBuilder::new()
//!     .http_to_bytes()  // Add this layer
//!     .bytes_to_json()  // Continue pipeline
//!     .service(inner_service);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! HTTP Request
//!     ↓ Extract body as bytes
//!     ↓ Extend Extensions (child layer)
//! Bytes Request → Inner Service
//!     ↓ Bytes Response
//!     ↓ Convert to HTTP response with streaming body
//!     ↓ Return original Extensions
//! HTTP Response
//! ```
//!
//! ## Extensions Handling
//!
//! This layer follows the standard Extensions pattern:
//! - Creates an **extended** Extensions layer for the inner service using `extend()`
//! - Inner service receives extended Extensions with access to parent context
//! - Response returns the **original** Extensions from the HTTP request
//! - Parent values always take precedence over inner service values
//!
//! ## Error Handling
//!
//! The layer can produce `HttpToBytesError` in these situations:
//! - **HTTP Response Building**: When converting bytes response back to HTTP response fails
//!
//! ## Performance Considerations
//!
//! - **Body Collection**: Collects the entire HTTP body into memory before processing
//! - **Async Body Handling**: Uses async body collection, requiring service cloning
//! - **Streaming Response**: Converts bytes response back to streaming HTTP body
//! - **Extensions Cloning**: Efficiently handles Extensions conversion via Arc references

use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::http_server::{Request as HttpRequest, Response as HttpResponse};
use futures::StreamExt;
use http_body::Frame;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// HTTP response building failed
    #[error("HTTP response building failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_HTTP_TO_BYTES_RESPONSE_BUILD_ERROR),
        help("Check that the HTTP response parameters are valid")
    )]
    HttpResponseBuilder {
        #[source]
        http_error: http::Error,
        #[extension("responseContext")]
        context: String,
    },
}

/// A Tower layer that transforms HTTP requests into bytes requests.
///
/// This layer sits early in the server-side request pipeline and is responsible for:
/// - Extracting HTTP request bodies as bytes
/// - Converting HTTP requests to bytes requests for downstream processing
/// - Managing Extensions hierarchy correctly
/// - Converting bytes responses back to streaming HTTP responses
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::http_to_bytes::HttpToBytesLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (inner_service, _handle) = tower_test::mock::spawn();
/// let layer = HttpToBytesLayer;
/// let service = layer.layer(inner_service);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct HttpToBytesLayer;

impl<S> Layer<S> for HttpToBytesLayer {
    type Service = HttpToBytesService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpToBytesService { inner: service }
    }
}

/// The service implementation that performs the HTTP to bytes transformation.
///
/// This service:
/// 1. Extracts the HTTP request body as bytes
/// 2. Creates an extended Extensions layer for the inner service  
/// 3. Calls the inner service with a bytes request
/// 4. Converts the bytes response back to a streaming HTTP response
/// 5. Returns the original Extensions in the HTTP response
#[derive(Clone, Debug)]
pub struct HttpToBytesService<S> {
    inner: S,
}

impl<S> Service<HttpRequest> for HttpToBytesService<S>
where
    S: Service<BytesRequest, Response = BytesResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        use std::mem;

        let inner = self.inner.clone();
        // Clone is required here because we need to do async body collection
        // before calling the service, unlike bytes_to_json which can parse JSON synchronously
        let mut inner = mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            // Convert HTTP request to bytes request
            let (parts, body) = req.into_parts();
            // Since body error type is now Infallible, collection cannot fail
            let body_bytes = body.collect().await.unwrap().to_bytes();

            // Convert http::Extensions directly to our Extensions
            let original_extensions: crate::Extensions = parts.extensions.into();

            // Create an extended layer for the inner service
            let extended_extensions = original_extensions.extend();

            let bytes_req = BytesRequest {
                extensions: extended_extensions,
                body: body_bytes,
            };

            // Call the inner service
            let bytes_resp = inner.call(bytes_req).await.map_err(Into::into)?;

            // Convert bytes response to HTTP response
            let mut http_resp = http::Response::builder()
                .status(200)
                .body(UnsyncBoxBody::new(http_body_util::StreamBody::new(
                    bytes_resp.responses.map(|chunk| Ok(Frame::data(chunk))),
                )))
                .map_err(|http_error| Error::HttpResponseBuilder {
                    http_error,
                    context: "Building HTTP response from bytes stream".to_string(),
                })?;

            // Convert original Extensions back to http::Extensions
            let http_extensions: http::Extensions = original_extensions.into();
            *http_resp.extensions_mut() = http_extensions;

            Ok(http_resp)
        })
    }
}
