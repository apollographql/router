use futures::stream;
use std::sync::Arc;

/// Router core json_client types
pub(super) use apollo_router_core::services::json_client::{Request as CoreJsonRequest, Response as CoreJsonResponse};

/// Router subgraph types
use crate::services::subgraph::{Request as SubgraphRequest, Response as SubgraphResponse, SubgraphRequestId};
use crate::{Context, graphql};
use crate::query_planner::fetch::OperationKind;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::spec::QueryHash;
use apollo_compiler::validation::Valid;

/// Metadata container for subgraph request/response information
/// Used to store and retrieve subgraph-specific data in extensions
#[derive(Debug, Clone)]
pub(crate) struct SubgraphMetadata {
    pub operation_kind: OperationKind,
    pub subgraph_name: String,
    pub id: SubgraphRequestId,
    pub authorization: Option<CacheKeyMetadata>,
    pub query_hash: Option<QueryHash>,
    pub executable_document: Option<Valid<apollo_compiler::ExecutableDocument>>,
}

impl SubgraphMetadata {
    /// Create SubgraphMetadata from a SubgraphRequest
    pub(crate) fn from_request(request: &SubgraphRequest) -> Self {
        Self {
            operation_kind: request.operation_kind,
            subgraph_name: request.subgraph_name.clone(),
            id: request.id.clone(),
            authorization: Arc::try_unwrap(request.authorization.clone()).ok(),
            query_hash: Arc::try_unwrap(request.query_hash.clone()).ok(),
            executable_document: request.executable_document.as_ref()
                .and_then(|doc| Arc::try_unwrap(doc.clone()).ok()),
        }
    }

    /// Create SubgraphMetadata from a SubgraphResponse
    pub(crate) fn from_response(response: &SubgraphResponse) -> Self {
        Self {
            operation_kind: OperationKind::Query, // Default for responses
            subgraph_name: response.subgraph_name.clone(),
            id: response.id.clone(),
            authorization: None,
            query_hash: None,
            executable_document: None,
        }
    }

    /// Extract SubgraphMetadata from extensions, providing reasonable defaults
    pub(crate) fn from_extensions(extensions: &apollo_router_core::Extensions) -> Self {
        extensions.get::<SubgraphMetadata>().unwrap_or_else(|| Self {
            operation_kind: OperationKind::Query,
            subgraph_name: "unknown".to_string(),
            id: SubgraphRequestId::default(),
            authorization: None,
            query_hash: None,
            executable_document: None,
        })
    }
}

// Convert from Router Core JsonRequest to Router SubgraphRequest
impl From<CoreJsonRequest> for SubgraphRequest {
    fn from(core_request: CoreJsonRequest) -> Self {
        // Extract context from extensions if present, otherwise create new
        let context = core_request
            .extensions
            .get::<Context>()
            .unwrap_or_else(Context::new);
        
        // Extract subgraph metadata from extensions
        let metadata = SubgraphMetadata::from_extensions(&core_request.extensions);
        
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
            operation_kind: metadata.operation_kind,
            context,
            subgraph_name: metadata.subgraph_name,
            subscription_stream: None,
            connection_closed_signal: None,
            query_hash: metadata.query_hash.map(Arc::new).unwrap_or_default(),
            authorization: metadata.authorization.map(Arc::new).unwrap_or_default(),
            executable_document: metadata.executable_document.map(Arc::new),
            id: metadata.id,
        }
    }
}

// Convert from Router SubgraphRequest to Router Core JsonRequest  
impl From<SubgraphRequest> for CoreJsonRequest {
    fn from(subgraph_request: SubgraphRequest) -> Self {
        // Serialize the GraphQL request body to JSON
        let json_body = serde_json::to_value(subgraph_request.subgraph_request.body())
            .unwrap_or_default();
        
        // Create subgraph metadata before moving fields out of the struct
        let metadata = SubgraphMetadata::from_request(&subgraph_request);
        
        // Create new extensions and populate with data
        let mut extensions = apollo_router_core::Extensions::new();
        
        // Store context
        extensions.insert(subgraph_request.context);
        
        // Store subgraph metadata as a single struct
        extensions.insert(metadata);
        
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
        
        // Extract subgraph metadata from extensions
        let metadata = SubgraphMetadata::from_extensions(&core_response.extensions);
        
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
            subgraph_name: metadata.subgraph_name,
            id: metadata.id,
        }
    }
}

// Convert from Router SubgraphResponse to Router Core JsonResponse
impl From<SubgraphResponse> for CoreJsonResponse {
    fn from(subgraph_response: SubgraphResponse) -> Self {
        // Serialize the GraphQL response to JSON
        let json_response = serde_json::to_value(subgraph_response.response.body())
            .unwrap_or_default();
        
        // Create subgraph metadata before moving fields out of the struct
        let metadata = SubgraphMetadata::from_response(&subgraph_response);
        
        // Create new extensions and populate with data
        let mut extensions = apollo_router_core::Extensions::new();
        
        // Store context
        extensions.insert(subgraph_response.context);
        
        // Store subgraph metadata as a single struct
        extensions.insert(metadata);
        
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