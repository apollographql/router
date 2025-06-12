//! # Bytes to JSON Layer
//!
//! The `BytesToJsonLayer` transforms bytes requests into JSON requests in the Apollo Router Core 
//! request pipeline. This layer is responsible for parsing bytes as JSON and converting them into 
//! structured JSON requests that can be processed by GraphQL services.
//!
//! ## Purpose
//!
//! - **JSON Parsing**: Deserializes bytes into structured JSON values
//! - **Request Type Transformation**: Converts `BytesRequest` to `JsonRequest`
//! - **Extensions Management**: Properly handles Extensions using `clone()` pattern
//! - **Error Handling**: Provides detailed error reporting for JSON parsing failures
//! - **Fail-Fast Design**: Validates JSON synchronously to catch errors early
//!
//! ## Usage
//!
//! The layer is typically used in the server-side request pipeline after HTTP body extraction:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (json_service, _handle) = tower_test::mock::spawn();
//! let service = ServiceBuilder::new()
//!     .http_to_bytes()    // Extract HTTP body as bytes
//!     .bytes_to_json()    // Parse bytes as JSON
//!     .service(json_service);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! Bytes Request
//!     ↓ Parse bytes as JSON (fail-fast)
//!     ↓ Extend Extensions (child layer)
//! JSON Request → Inner Service
//!     ↓ JSON Response (stream)
//!     ↓ Serialize JSON values back to bytes
//!     ↓ Return original Extensions
//! Bytes Response
//! ```
//!
//! ## Extensions Handling
//!
//! This layer propagates Extensions without cloning, as there is only one inner request.
//!
//! ## Error Handling
//!
//! The layer can produce `BytesToJsonError` in these situations:
//! - **JSON Deserialization**: When bytes cannot be parsed as valid JSON
//! - **Position Information**: Provides source spans for detailed error reporting
//! - **Input Preservation**: Includes the invalid input data for debugging
//!
//! ## Performance Considerations
//!
//! - **Synchronous Parsing**: JSON parsing happens synchronously for fail-fast behavior
//! - **No Service Cloning**: More efficient than HTTP layer as no async body collection needed
//! - **Stream Processing**: Handles JSON response streams efficiently
//! - **Fallback Handling**: Uses empty JSON object `{}` as fallback for serialization errors

use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use bytes::Bytes;
use futures::StreamExt;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// Bytes to JSON conversion failed
    #[error("Bytes to JSON conversion failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_TO_JSON_CONVERSION_ERROR),
        help("Ensure the input is valid JSON")
    )]
    JsonDeserialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
    },
}

/// A Tower layer that transforms bytes requests into JSON requests.
///
/// This layer sits in the server-side request pipeline after bytes extraction and is responsible for:
/// - Parsing bytes as JSON with fail-fast error handling
/// - Converting bytes requests to JSON requests for GraphQL processing
/// - Managing Extensions hierarchy correctly
/// - Converting JSON responses back to bytes responses
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::bytes_to_json::BytesToJsonLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (json_service, _handle) = tower_test::mock::spawn();
/// let layer = BytesToJsonLayer;
/// let service = layer.layer(json_service);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct BytesToJsonLayer;

impl<S> Layer<S> for BytesToJsonLayer {
    type Service = BytesToJsonService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesToJsonService { inner: service }
    }
}

/// The service implementation that performs the bytes to JSON transformation.
///
/// This service:
/// 1. Parses the bytes as JSON synchronously (fail-fast)
/// 2. Creates an extended Extensions layer for the inner service
/// 3. Calls the inner service with a JSON request
/// 4. Converts the JSON response stream back to bytes
/// 5. Returns the original Extensions in the bytes response
#[derive(Clone, Debug)]
pub struct BytesToJsonService<S> {
    inner: S,
}

impl<S> Service<BytesRequest> for BytesToJsonService<S>
where
    S: Service<JsonRequest, Response = JsonResponse> + Send + 'static,
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
        // Convert bytes to JSON synchronously - fail fast if invalid
        let json_body = match serde_json::from_slice(&req.body) {
            Ok(json) => json,
            Err(json_error) => {
                let input_data = String::from_utf8_lossy(&req.body).into_owned();
                let error = Error::JsonDeserialization {
                    json_error,
                    input_data: Some(input_data),
                };
                return Box::pin(async move { Err(error.into()) });
            }
        };

        let json_req = JsonRequest {
            extensions: req.extensions,
            body: json_body,
        };

        // Call the inner service directly - no cloning needed
        let future = self.inner.call(json_req);

        Box::pin(async move {
            // Await the inner service call
            let json_resp = future.await.map_err(Into::into)?;

            // Convert JSON response to bytes response
            let bytes_stream = json_resp.responses.map(|json_result| {
                match json_result {
                    Ok(json_value) => {
                        // Try to serialize the JSON value to bytes
                        match serde_json::to_vec(&json_value) {
                            Ok(bytes) => Ok(Bytes::from(bytes)),
                            Err(e) => Err(e.into()), // Return serialization error instead of defaulting
                        }
                    }
                    Err(e) => Err(e), // Propagate upstream errors
                }
            });

            let bytes_resp = BytesResponse {
                extensions: json_resp.extensions,
                responses: Box::pin(bytes_stream),
            };

            Ok(bytes_resp)
        })
    }
}

#[cfg(test)]
mod tests;
