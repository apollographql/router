//! # Prepare Query Layer
//!
//! The `PrepareQueryLayer` is a **composite layer** that orchestrates GraphQL query preparation
//! by combining query parsing and query planning services. This layer transforms JSON requests
//! containing GraphQL queries into execution requests ready for query execution.
//!
//! ## Purpose
//!
//! - **Query Orchestration**: Coordinates query parsing and planning services
//! - **Request Type Transformation**: Converts `JsonRequest` to `ExecutionRequest`
//! - **GraphQL Processing**: Handles GraphQL query strings, operation names, and variables
//! - **Extensions Management**: Properly handles Extensions using `clone()` pattern
//! - **Error Aggregation**: Consolidates errors from parsing and planning phases
//!
//! ## Architecture
//!
//! This is a **composite layer** that internally uses two services:
//! - **Query Parse Service**: Parses GraphQL query strings into executable documents
//! - **Query Plan Service**: Creates execution plans from parsed queries
//!
//! Unlike atomic layers, this layer orchestrates multiple service calls to provide
//! a higher-level abstraction for the complete query preparation phase.
//!
//! ## Usage
//!
//! The layer requires both parsing and planning services to be configured:
//!
//! ```rust,ignore
//! use apollo_router_core::layers::ServiceBuilderExt;
//! use tower::ServiceBuilder;
//!
//! # fn example() {
//! # let (query_parse_service, _handle1) = tower_test::mock::spawn();
//! # let (query_plan_service, _handle2) = tower_test::mock::spawn();
//! # let (execution_service, _handle3) = tower_test::mock::spawn();
//! let service = ServiceBuilder::new()
//!     .bytes_to_json()  // JSON parsing
//!     .prepare_query(   // Query preparation
//!         query_parse_service,
//!         query_plan_service
//!     )
//!     .service(execution_service);
//! # }
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! JSON Request (GraphQL query)
//!     ↓ Extract query string, operation name, variables
//!     ↓ Extend Extensions (child layer)
//!     ↓ Call query_parse_service
//! ExecutableDocument
//!     ↓ Call query_plan_service  
//! QueryPlan
//!     ↓ Combine into ExecutionRequest
//! Execution Request → Inner Service
//!     ↓ Execution Response
//!     ↓ Transform to JSON Response
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
//! The layer can produce `PrepareQueryError` in these situations:
//! - **Orchestration Errors**: When service coordination fails
//! - **JSON Extraction**: When required GraphQL fields are missing from JSON
//! - **Query Plan Service**: When query planning fails after successful parsing
//!
//! ## GraphQL Request Format
//!
//! The layer expects JSON requests with standard GraphQL structure:
//!
//! ```json
//! {
//!   "query": "query GetUser($id: ID!) { user(id: $id) { name } }",
//!   "operationName": "GetUser",
//!   "variables": { "id": "123" }
//! }
//! ```
//!
//! ## Performance Considerations
//!
//! - **Service Orchestration**: Requires sequential calls to parse and plan services
//! - **Composite Nature**: More complex than atomic layers due to multi-service coordination
//! - **Error Propagation**: Handles errors from multiple service boundaries
//! - **Future Enhancement**: Currently uses placeholder logic pending full implementation

use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use crate::services::query_execution::{Request as ExecutionRequest, Response as ExecutionResponse};
use crate::services::query_parse;
use crate::services::query_plan;
use std::pin::Pin;
use tower::BoxError;
use tower::{Layer, Service};

#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum Error {
    /// JSON extraction failed during query preparation
    #[error("JSON extraction failed during query preparation")]
    #[diagnostic(
        code(APOLLO_ROUTER_LAYERS_PREPARE_QUERY_JSON_EXTRACTION_ERROR),
        help("Ensure the JSON request contains valid GraphQL query fields")
    )]
    JsonExtraction {
        #[extension("field")]
        missing_field: String,
        #[source_code]
        request_body: Option<String>,
    },
}

/// A composite Tower layer that orchestrates GraphQL query preparation.
///
/// This layer combines query parsing and query planning services to transform
/// JSON requests containing GraphQL queries into execution requests. It acts as
/// a higher-level abstraction that coordinates multiple services internally.
///
/// # Type Parameters
///
/// * `P` - The query parse service type
/// * `Pl` - The query plan service type
///
/// # Example
///
/// ```rust,ignore
/// use apollo_router_core::layers::prepare_query::PrepareQueryLayer;
/// use tower::{Layer, ServiceExt};
///
/// # fn example() {
/// # let (parse_service, _handle1) = tower_test::mock::spawn();
/// # let (plan_service, _handle2) = tower_test::mock::spawn();
/// # let (execution_service, _handle3) = tower_test::mock::spawn();
/// let layer = PrepareQueryLayer::new(parse_service, plan_service);
/// let service = layer.layer(execution_service);
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct PrepareQueryLayer<P, Pl> {
    query_parse_service: P,
    query_plan_service: Pl,
}

impl<P, Pl> PrepareQueryLayer<P, Pl> {
    pub fn new(query_parse_service: P, query_plan_service: Pl) -> Self {
        Self {
            query_parse_service,
            query_plan_service,
        }
    }
}

impl<S, P, Pl> Layer<S> for PrepareQueryLayer<P, Pl> 
where
    P: Clone,
    Pl: Clone,
{
    type Service = PrepareQueryService<S, P, Pl>;

    fn layer(&self, service: S) -> Self::Service {
        PrepareQueryService { 
            inner: service,
            query_parse_service: self.query_parse_service.clone(),
            query_plan_service: self.query_plan_service.clone(),
        }
    }
}

/// The service implementation that orchestrates query preparation.
///
/// This service:
/// 1. Extracts GraphQL query details from JSON requests
/// 2. Creates an extended Extensions layer for the inner service
/// 3. Coordinates calls to query parse and plan services (currently placeholder)
/// 4. Calls the inner execution service with the prepared request
/// 5. Transforms execution responses back to JSON responses
/// 6. Returns the original Extensions in the JSON response
///
/// # Type Parameters
///
/// * `S` - The inner execution service type
/// * `P` - The query parse service type
/// * `Pl` - The query plan service type
#[derive(Clone, Debug)]
pub struct PrepareQueryService<S, P, Pl> {
    inner: S,
    query_parse_service: P,
    query_plan_service: Pl,
}

impl<S, P, Pl> Service<JsonRequest> for PrepareQueryService<S, P, Pl>
where
    S: Service<ExecutionRequest, Response = ExecutionResponse> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    P: Service<query_parse::Request, Response = query_parse::Response> + Clone + Send + 'static,
    P::Future: Send + 'static,
    P::Error: Into<BoxError>,
    Pl: Service<query_plan::Request, Response = query_plan::Response> + Clone + Send + 'static,
    Pl::Future: Send + 'static,
    Pl::Error: Into<BoxError>,
{
    type Response = JsonResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: JsonRequest) -> Self::Future {
        // This layer orchestrates query_parse and query_plan services
        // Flow: JSON Request → query_parse → ExecutableDocument → query_plan → QueryPlan → Execution Request
        
        // Create an extended layer for the inner service
        let original_extensions = req.extensions;
        let cloned_extensions = original_extensions.clone();

        // Extract query string, operation name and variables from JSON body
        let (_query_string, operation_name, query_variables) = match extract_query_details(&req.body) {
            Ok(details) => details,
            Err(e) => return Box::pin(async move { Err(e.into()) }),
        };

        // Clone services for async usage (will be used in future implementation)
        let _query_parse_service = self.query_parse_service.clone();
        let _query_plan_service = self.query_plan_service.clone();
        let mut inner_service = self.inner.clone();

        Box::pin(async move {
            // TODO: For now, create a placeholder query plan until we implement proper service orchestration
            // In a complete implementation, this would:
            // 1. Call query_parse_service.call(parse_req).await to parse the GraphQL query
            // 2. Call query_plan_service.call(plan_req).await to create the query plan
            // 3. Combine the results into an ExecutionRequest
            
            // Create a placeholder query plan - this will be replaced with actual service calls
            let query_plan = apollo_federation::query_plan::QueryPlan::default();

            let execution_req = ExecutionRequest {
                extensions: cloned_extensions,
                operation_name,
                query_plan,
                query_variables,
            };

            // Call the inner execution service
            let execution_resp = inner_service.call(execution_req).await.map_err(Into::into)?;

            // Transform ExecutionResponse back to JsonResponse
            let json_resp = JsonResponse {
                extensions: original_extensions,
                responses: execution_resp.responses,
            };

            Ok(json_resp)
        })
    }
}

fn extract_query_details(json_body: &crate::json::JsonValue) -> Result<(serde_json::Value, Option<String>, std::collections::HashMap<String, serde_json::Value>), Error> {
    // Extract the GraphQL query string
    let query_string = json_body.get("query")
        .ok_or_else(|| Error::JsonExtraction {
            missing_field: "query".to_string(),
            request_body: Some(json_body.to_string()),
        })?
        .clone();

    // Extract operation name (optional)
    let operation_name = json_body.get("operationName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Extract variables (optional)
    let query_variables = json_body.get("variables")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    Ok((query_string, operation_name, query_variables))
}

#[cfg(test)]
mod tests; 