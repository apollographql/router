use futures::stream;
use std::sync::Arc;

/// Router core json_client types
pub(super) use apollo_router_core::services::json_client::{Request as CoreJsonRequest, Response as CoreJsonResponse};

/// Router subgraph types
use crate::services::subgraph::{Request as SubgraphRequest, Response as SubgraphResponse, SubgraphRequestId};
use crate::{Context, graphql};
use crate::query_planner::fetch::OperationKind;

// Convert from Router Core JsonRequest to Router SubgraphRequest
impl From<CoreJsonRequest> for SubgraphRequest {
    fn from(core_request: CoreJsonRequest) -> Self {
        // Extract context from extensions if present, otherwise create new
        let context = core_request
            .extensions
            .get::<Context>()
            .unwrap_or_else(Context::new);
        
        // Try to deserialize the JSON body as a GraphQL request
        let graphql_request = serde_json::from_value::<graphql::Request>(core_request.body)
            .unwrap_or_default();
        
        // Create a basic HTTP request wrapper for the GraphQL request
        let subgraph_request = http::Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json")
            .body(graphql_request.clone())
            .expect("building HTTP request should not fail");
        
        // Extract additional fields from extensions or use defaults
        let operation_kind = core_request.extensions
            .get::<OperationKind>()
            .unwrap_or(OperationKind::Query);
        
        let subgraph_name = core_request.extensions
            .get::<String>()
            .unwrap_or_else(|| "unknown".to_string());
        
        let request_id = core_request.extensions
            .get::<SubgraphRequestId>()
            .unwrap_or_default();
        
        // Create the supergraph request (required field) - use the same GraphQL request
        let supergraph_request = Arc::new(
            http::Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(graphql_request.clone())
                .expect("building supergraph HTTP request should not fail")
        );
        
        SubgraphRequest {
            supergraph_request,
            subgraph_request,
            operation_kind,
            context,
            subgraph_name,
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: Default::default(),
            authorization: Default::default(),
            executable_document: None,
            id: request_id,
        }
    }
}

// Convert from Router SubgraphRequest to Router Core JsonRequest  
impl From<SubgraphRequest> for CoreJsonRequest {
    fn from(subgraph_request: SubgraphRequest) -> Self {
        // Serialize the GraphQL request body to JSON
        let json_body = serde_json::to_value(subgraph_request.subgraph_request.body())
            .unwrap_or_default();
        
        // Create new extensions and populate with subgraph request data
        let mut extensions = apollo_router_core::Extensions::new();
        
        // Store context
        extensions.insert(subgraph_request.context);
        
        // Store additional fields that might be needed for round-trip conversion
        extensions.insert(subgraph_request.operation_kind);
        extensions.insert(subgraph_request.subgraph_name);
        extensions.insert(subgraph_request.id);
        
        if let Some(auth) = Arc::try_unwrap(subgraph_request.authorization).ok() {
            extensions.insert(auth);
        }
        
        if let Some(query_hash) = Arc::try_unwrap(subgraph_request.query_hash).ok() {
            extensions.insert(query_hash);
        }
        
        if let Some(doc) = subgraph_request.executable_document {
            if let Ok(doc) = Arc::try_unwrap(doc) {
                extensions.insert(doc);
            }
        }
        
        CoreJsonRequest {
            extensions,
            body: json_body,
        }
    }
}

// Convert from Router Core JsonResponse to Router SubgraphResponse
impl From<CoreJsonResponse> for SubgraphResponse {
    fn from(core_response: CoreJsonResponse) -> Self {
        // Extract context from extensions if present, otherwise create new
        let context = core_response
            .extensions
            .get::<Context>()
            .unwrap_or_else(Context::new);
        
        // Extract subgraph name from extensions or use default
        let subgraph_name = core_response.extensions
            .get::<String>()
            .unwrap_or_else(|| "unknown".to_string());
        
        // Extract request ID from extensions or use default
        let id = core_response.extensions
            .get::<SubgraphRequestId>()
            .unwrap_or_default();
        
        // For now, we'll create a default GraphQL response and note that this
        // conversion loses the streaming nature. In a real implementation, you might
        // want to collect all stream items or handle streaming responses differently.
        let graphql_response = graphql::Response::default();
        
        // Create HTTP response wrapper
        let http_response = http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(graphql_response)
            .expect("building HTTP response should not fail");
        
        SubgraphResponse {
            response: http_response,
            context,
            subgraph_name,
            id,
        }
    }
}

// Convert from Router SubgraphResponse to Router Core JsonResponse
impl From<SubgraphResponse> for CoreJsonResponse {
    fn from(subgraph_response: SubgraphResponse) -> Self {
        // Serialize the GraphQL response to JSON
        let json_response = serde_json::to_value(subgraph_response.response.body())
            .unwrap_or_default();
        
        // Create new extensions and populate with subgraph response data
        let mut extensions = apollo_router_core::Extensions::new();
        
        // Store context
        extensions.insert(subgraph_response.context);
        
        // Store additional fields for round-trip conversion
        extensions.insert(subgraph_response.subgraph_name);
        extensions.insert(subgraph_response.id);
        
        // Create a stream with just this one response
        let response_stream = stream::once(async move { Ok(json_response) });
        
        CoreJsonResponse {
            extensions,
            responses: Box::pin(response_stream),
        }
    }
}

#[cfg(test)]
mod tests; 