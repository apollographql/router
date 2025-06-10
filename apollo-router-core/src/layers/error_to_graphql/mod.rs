//! # Error to GraphQL Layer
//!
//! The `ErrorToGraphQLLayer` is a utility layer that transforms service errors into 
//! GraphQL-compliant error responses. This layer catches errors from downstream services
//! and converts them into properly formatted GraphQL error responses following the 
//! GraphQL specification.
//!
//! ## Purpose
//!
//! - **Error Transformation**: Converts service errors to GraphQL error format
//! - **Error Response Generation**: Creates proper GraphQL error responses with null data
//! - **Error Standardization**: Ensures all errors follow GraphQL specification format
//! - **Extensions Preservation**: Maintains error extensions for client debugging
//! - **Service Protection**: Prevents raw service errors from being exposed to clients
//!
//! ## Usage
//!
//! The layer is typically used at the top of service stacks to catch and format errors:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::error_to_graphql::ErrorToGraphQLLayer;
//! use tower::{ServiceBuilder, Layer};
//!
//! # fn example() {
//! # let (graphql_service, _handle) = tower_test::mock::spawn();
//! let service = ServiceBuilder::new()
//!     .layer(ErrorToGraphQLLayer)  // Catch and format errors
//!     .service(graphql_service);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! Any Request Type
//!     ↓ Pass request through to inner service
//! Inner Service Call
//!     ↓ Success: Pass JSON response through
//!     ↓ Error: Convert to GraphQL error format
//! GraphQL Error Response (JSON)
//! {
//!   "data": null,
//!   "errors": [{ "message": "...", "extensions": {...} }],
//!   "extensions": {}
//! }
//! ```
//!
//! ## GraphQL Error Format
//!
//! The layer converts errors to GraphQL specification-compliant format:
//!
//! ```json
//! {
//!   "data": null,
//!   "errors": [
//!     {
//!       "message": "Error description",
//!       "extensions": {
//!         "code": "ERROR_CODE",
//!         "additionalField": "value"
//!       }
//!     }
//!   ],
//!   "extensions": {}
//! }
//! ```
//!
//! ## Error Processing
//!
//! The layer uses the `HeapErrorToGraphQL` trait to convert errors:
//! - **Service Errors**: Automatically converted using the error system's GraphQL integration
//! - **Extensions Extraction**: Error extensions are preserved in the GraphQL error
//! - **Message Formatting**: Error messages are formatted for client consumption
//! - **Code Preservation**: Error codes are maintained for programmatic error handling
//!
//! ## Extensions Handling
//!
//! This layer creates default Extensions for error responses:
//! - **Error Response**: Uses default Extensions when creating GraphQL error responses
//! - **Success Response**: Passes through successful responses unchanged
//! - **Extension Isolation**: Error responses don't inherit request Extensions
//!
//! ## Performance Considerations
//!
//! - **Error Path Only**: No overhead for successful requests
//! - **Single Stream Response**: Creates single-item JSON response stream for errors
//! - **Lightweight Conversion**: Efficient error-to-JSON transformation
//! - **Memory Efficiency**: Only allocates for error responses, not successful ones

use crate::services::json_server::{Response as JsonResponse, ResponseStream};
use apollo_router_error::HeapErrorToGraphQL;
use futures::stream;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};





/// A Tower layer that transforms service errors into GraphQL-compliant error responses.
///
/// This utility layer catches errors from downstream services and converts them into
/// properly formatted GraphQL error responses that follow the GraphQL specification.
/// Successful responses are passed through unchanged.
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::error_to_graphql::ErrorToGraphQLLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (graphql_service, _handle) = tower_test::mock::spawn();
/// let layer = ErrorToGraphQLLayer;
/// let service = layer.layer(graphql_service);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct ErrorToGraphQLLayer;

impl<S> Layer<S> for ErrorToGraphQLLayer {
    type Service = ErrorToGraphQLService<S>;

    fn layer(&self, service: S) -> Self::Service {
        ErrorToGraphQLService { inner: service }
    }
}

/// The service implementation that performs error to GraphQL transformation.
///
/// This service:
/// 1. Passes requests through to the inner service unchanged
/// 2. On success: Returns the inner service response as-is
/// 3. On error: Converts the error to GraphQL format using `HeapErrorToGraphQL` trait
/// 4. Creates a GraphQL-compliant JSON response with null data and formatted errors
/// 5. Returns the error response with default Extensions
#[derive(Clone, Debug)]
pub struct ErrorToGraphQLService<S> {
    inner: S,
}

impl<S, Req> Service<Req> for ErrorToGraphQLService<S>
where
    S: Service<Req, Response = JsonResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let future = self.inner.call(req);

        Box::pin(async move {
            match future.await {
                Ok(response) => {
                    // For successful responses, pass them through
                    Ok(response)
                }
                Err(service_error) => {
                    let boxed_error: BoxError = service_error.into();

                    // Convert the error to GraphQL format using the HeapErrorToGraphQL trait
                    let graphql_error = boxed_error.to_graphql_error();

                    // Create GraphQL response structure
                    let graphql_response = serde_json::json!({
                        "data": null,
                        "errors": [graphql_error],
                        "extensions": {}
                    });

                    let response_stream: ResponseStream =
                        Box::pin(stream::once(async move { Ok(graphql_response) }));

                    Ok(JsonResponse {
                        extensions: Default::default(),
                        responses: response_stream,
                    })
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
