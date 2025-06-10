//! # Bytes Client to JSON Client Layer
//!
//! The `BytesClientToJsonClientLayer` transforms bytes client requests into JSON client requests 
//! in the Apollo Router Core client-side pipeline. This layer is responsible for deserializing
//! bytes into JSON format for client services and serializing JSON responses back to bytes.
//!
//! ## Purpose
//!
//! - **JSON Deserialization**: Converts bytes data into structured JSON values
//! - **Request Type Transformation**: Converts `BytesClientRequest` to `JsonClientRequest`
//! - **Client-Side Processing**: Enables JSON-based client service processing
//! - **Response Serialization**: Converts JSON responses back to bytes format
//! - **Error Handling**: Provides detailed error reporting for JSON processing failures
//!
//! ## Usage
//!
//! The layer is typically used in client-side pipelines after bytes abstraction:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (json_client, _handle) = tower_test::mock::spawn();
//! let client = ServiceBuilder::new()
//!     .http_client_to_bytes_client()    // HTTP to bytes
//!     .bytes_client_to_json_client()    // Bytes to JSON
//!     .service(json_client);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! Bytes Client Request
//!     ↓ Deserialize bytes to JSON (fail-fast)
//!     ↓ Create default Extensions
//! JSON Client Request → Inner Service
//!     ↓ JSON Client Response
//!     ↓ Serialize JSON values back to bytes
//!     ↓ Return default Extensions
//! Bytes Client Response
//! ```
//!
//! ## Extensions Handling
//!
//! This layer follows the standard Extensions pattern:
//! - Creates an **extended** Extensions layer for the inner service using `extend()`
//! - Inner service receives extended Extensions with access to parent context
//! - Response returns the **original** Extensions from the bytes client request
//! - Parent values always take precedence over inner service values
//!
//! ## Error Handling
//!
//! The layer can produce `BytesClientToJsonClientError` in these situations:
//! - **JSON Deserialization**: When bytes cannot be parsed as valid JSON
//! - **JSON Serialization**: When JSON values cannot be serialized back to bytes
//! - **Content Preservation**: Includes problematic data for debugging
//! - **Context Information**: Provides processing context for error reporting
//!
//! ## Performance Considerations
//!
//! - **Synchronous Processing**: JSON operations happen synchronously for fail-fast behavior
//! - **Memory Efficiency**: Direct bytes-to-JSON conversion without intermediate steps
//! - **Fallback Handling**: Uses empty JSON object `{}` as fallback for serialization errors
//! - **Client Pipeline Position**: Positioned after protocol abstraction layers

use crate::services::bytes_client::{Request as BytesClientRequest, Response as BytesClientResponse};
use crate::services::json_client::{Request as JsonClientRequest, Response as JsonClientResponse};
use futures::StreamExt;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// Bytes to JSON deserialization failed in client layer
    #[error("Bytes to JSON deserialization failed in client layer")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_CLIENT_TO_JSON_CLIENT_DESERIALIZATION_ERROR),
        help("Ensure the bytes contain valid JSON data")
    )]
    JsonDeserialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        input_data: Option<String>,
        #[extension("deserializationContext")]
        context: String,
    },

    /// JSON to bytes serialization failed in response transformation
    #[error("JSON to bytes serialization failed in response transformation")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_BYTES_CLIENT_TO_JSON_CLIENT_SERIALIZATION_ERROR),
        help("Ensure the JSON response can be serialized to bytes")
    )]
    JsonSerialization {
        #[source]
        json_error: serde_json::Error,
        #[source_code]
        json_content: Option<String>,
        #[extension("serializationContext")]
        context: String,
    },
}

/// A Tower layer that transforms bytes client requests into JSON client requests.
///
/// This layer sits in client-side pipelines and is responsible for:
/// - Deserializing bytes data to JSON with fail-fast error handling
/// - Converting bytes client requests to JSON client requests for processing
/// - Managing Extensions hierarchy correctly using extend() pattern
/// - Converting JSON responses back to bytes format
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::bytes_client_to_json_client::BytesClientToJsonClientLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (json_client, _handle) = tower_test::mock::spawn();
/// let layer = BytesClientToJsonClientLayer;
/// let service = layer.layer(json_client);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct BytesClientToJsonClientLayer;

impl<S> Layer<S> for BytesClientToJsonClientLayer {
    type Service = BytesClientToJsonClientService<S>;

    fn layer(&self, service: S) -> Self::Service {
        BytesClientToJsonClientService { inner: service }
    }
}

/// The service implementation that performs bytes client to JSON client transformation.
///
/// This service:
/// 1. Deserializes the bytes request body to JSON synchronously (fail-fast)
/// 2. Creates an extended Extensions layer for the inner service
/// 3. Calls the inner JSON client service
/// 4. Converts the JSON response stream back to bytes
/// 5. Returns the original Extensions in the bytes client response
#[derive(Clone, Debug)]
pub struct BytesClientToJsonClientService<S> {
    inner: S,
}

impl<S> Service<BytesClientRequest> for BytesClientToJsonClientService<S>
where
    S: Service<JsonClientRequest, Response = JsonClientResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = BytesClientResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: BytesClientRequest) -> Self::Future {
        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let extended_extensions = original_extensions.extend();

        // Convert bytes to JSON
        let json_body = match serde_json::from_slice(&req.body) {
            Ok(json) => json,
            Err(json_error) => {
                let error = Error::JsonDeserialization {
                    json_error,
                    input_data: Some(String::from_utf8_lossy(&req.body).into_owned()),
                    context: "Converting bytes request to JSON for client".to_string(),
                };
                return Box::pin(async move { Err(error.into()) });
            }
        };

        let json_client_req = JsonClientRequest {
            extensions: extended_extensions,
            body: json_body,
        };

        let future = self.inner.call(json_client_req);

        Box::pin(async move {
            // Await the inner service call
            let json_resp = future.await.map_err(Into::into)?;

            // Transform JsonClientResponse back to BytesClientResponse
            // Convert JSON responses back to bytes
            let bytes_responses = json_resp.responses.map(|json_result| {
                match json_result {
                    Ok(json_value) => {
                        // Try to serialize the JSON value to bytes
                        match serde_json::to_vec(&json_value) {
                            Ok(bytes) => Ok(bytes::Bytes::from(bytes)),
                            Err(e) => Err(e.into()), // Return serialization error instead of defaulting
                        }
                    }
                    Err(e) => Err(e), // Propagate upstream errors
                }
            });

            let bytes_resp = BytesClientResponse {
                extensions: original_extensions,
                responses: Box::pin(bytes_responses),
            };

            Ok(bytes_resp)
        })
    }
}

#[cfg(test)]
mod tests; 