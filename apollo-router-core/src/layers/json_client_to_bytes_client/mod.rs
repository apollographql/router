//! # JSON to Bytes Layer
//!
//! The `JsonToBytesLayer` transforms JSON client requests into bytes client requests in the 
//! Apollo Router Core client-side pipeline. This layer is responsible for serializing JSON 
//! values into bytes for transmission to downstream services or external APIs.
//!
//! ## Purpose
//!
//! - **JSON Serialization**: Converts JSON values into bytes representation
//! - **Request Type Transformation**: Converts `JsonRequest` to `BytesRequest` for client services
//! - **Extensions Management**: Properly handles Extensions using `clone()` pattern
//! - **Error Handling**: Provides detailed error reporting for JSON serialization failures
//! - **Fail-Fast Design**: Validates JSON serialization synchronously to catch errors early
//!
//! ## Usage
//!
//! The layer is typically used in client-side pipelines before bytes transmission:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (http_client, _handle) = tower_test::mock::spawn();
//! let client = ServiceBuilder::new()
//!     .json_to_bytes()      // Serialize JSON to bytes
//!     .bytes_to_http()      // Convert to HTTP requests
//!     .service(http_client);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! JSON Request (client-side)
//!     ↓ Serialize JSON to bytes (fail-fast)
//!     ↓ Extend Extensions (child layer)
//! Bytes Request → Inner Service
//!     ↓ Bytes Response (stream)
//!     ↓ Deserialize bytes back to JSON values
//!     ↓ Return original Extensions
//! JSON Response
//! ```
//!
//! ## Extensions Handling
//!
//! This layer follows the standard Extensions pattern:
//! - Creates a **cloned** Extensions layer for the inner service using `clone()`
//! - Inner service receives extended Extensions with access to parent context
//! - Response returns the **original** Extensions from the JSON request
//! - Parent values always take precedence over inner service values
//!
//! ## Error Handling
//!
//! The layer can produce `JsonToBytesError` in these situations:
//! - **JSON Serialization**: When JSON values cannot be serialized to bytes
//! - **Content Preservation**: Includes the problematic JSON content for debugging
//! - **Context Information**: Provides serialization context for error reporting
//!
//! ## Performance Considerations
//!
//! - **Synchronous Serialization**: JSON serialization happens synchronously for fail-fast behavior
//! - **No Service Cloning**: Efficient implementation without async complexity
//! - **Stream Processing**: Handles bytes response streams efficiently
//! - **Fallback Handling**: Uses empty JSON object `{}` as fallback for deserialization errors
//! - **Memory Efficiency**: Converts JSON directly to bytes without intermediate representations

use crate::services::bytes_client::{Request as BytesRequest, Response as BytesResponse};
use crate::services::json_client::{Request as JsonRequest, Response as JsonResponse};
use bytes::Bytes;
use futures::StreamExt;
use std::future::Future;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// JSON to bytes serialization failed
    #[error("JSON to bytes serialization failed")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_JSON_TO_BYTES_SERIALIZATION_ERROR),
        help("Ensure the JSON value can be serialized")
    )]
    JsonSerialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        json_content: Option<String>,
    },
}

/// A Tower layer that transforms JSON client requests into bytes client requests.
///
/// This layer sits in client-side pipelines and is responsible for:
/// - Serializing JSON values to bytes with fail-fast error handling
/// - Converting JSON requests to bytes requests for transmission
/// - Managing Extensions hierarchy correctly
/// - Converting bytes responses back to JSON responses
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::json_to_bytes::JsonToBytesLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (bytes_client, _handle) = tower_test::mock::spawn();
/// let layer = JsonToBytesLayer;
/// let service = layer.layer(bytes_client);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct JsonToBytesLayer;

impl<S> Layer<S> for JsonToBytesLayer {
    type Service = JsonToBytesService<S>;

    fn layer(&self, service: S) -> Self::Service {
        JsonToBytesService { inner: service }
    }
}

/// The service implementation that performs the JSON to bytes transformation.
///
/// This service:
/// 1. Serializes the JSON request body to bytes synchronously (fail-fast)
/// 2. Creates an extended Extensions layer for the inner service
/// 3. Calls the inner service with a bytes request
/// 4. Converts the bytes response stream back to JSON values
/// 5. Returns the original Extensions in the JSON response
#[derive(Clone, Debug)]
pub struct JsonToBytesService<S> {
    inner: S,
}

impl<S> Service<JsonRequest> for JsonToBytesService<S>
where
    S: Service<BytesRequest, Response = BytesResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: JsonRequest) -> Self::Future {
        // Convert JSON to bytes synchronously - fail fast if invalid
        let bytes_body = match serde_json::to_vec(&req.body) {
            Ok(bytes) => Bytes::from(bytes),
            Err(json_error) => {
                let error = Error::JsonSerialization {
                    json_error,
                    json_content: Some(req.body.to_string()),
                };
                return Box::pin(async move { Err(error.into()) });
            }
        };

        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let cloned_extensions = original_extensions.clone();

        let bytes_req = BytesRequest {
            extensions: cloned_extensions,
            body: bytes_body,
        };

        // Call the inner service directly - no cloning needed
        let future = self.inner.call(bytes_req);

        Box::pin(async move {
            // Await the inner service call
            let bytes_resp = future.await.map_err(Into::into)?;

            // Convert bytes response to JSON response
            let json_stream = bytes_resp.responses.map(|bytes_result| {
                match bytes_result {
                    Ok(bytes) => {
                        // Try to deserialize the bytes to JSON
                        match serde_json::from_slice(&bytes) {
                            Ok(json_value) => Ok(json_value),
                            Err(e) => Err(e.into()), // Return deserialization error instead of defaulting
                        }
                    }
                    Err(e) => Err(e), // Propagate upstream errors
                }
            });

            let json_resp = JsonResponse {
                extensions: original_extensions,
                responses: Box::pin(json_stream),
            };

            Ok(json_resp)
        })
    }
}

#[cfg(test)]
mod tests;
