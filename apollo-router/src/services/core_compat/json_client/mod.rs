use crate::Context;
use crate::graphql;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::query_planner::fetch::OperationKind;
use crate::services::subgraph::{
    BoxGqlStream, Request as SubgraphRequest, Response as SubgraphResponse, SubgraphRequestId,
};
use crate::spec::QueryHash;
use apollo_compiler::validation::Valid;
use futures;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tower::BoxError;

/// Router core json_client types
pub(super) use apollo_router_core::services::json_client::{
    Request as CoreJsonRequest, Response as CoreJsonResponse,
};

/// Metadata container for subgraph request/response information
/// Used to store and retrieve subgraph-specific data in extensions
#[derive(Debug)]
struct RequestMetadata {
    http_parts: http::request::Parts,
    supergraph_request: Arc<http::Request<graphql::Request>>,
    operation_kind: OperationKind,
    context: Context,
    subgraph_name: String,
    subscription_stream: Option<mpsc::Sender<BoxGqlStream>>,
    connection_closed_signal: Option<broadcast::Receiver<()>>,
    query_hash: Arc<QueryHash>,
    authorization: Arc<CacheKeyMetadata>,
    executable_document: Option<Arc<Valid<apollo_compiler::ExecutableDocument>>>,
    id: SubgraphRequestId,
}

/// Metadata for storing SubgraphResponse information in extensions during conversion
#[derive(Debug)]
struct ResponseMetadata {
    http_parts: http::response::Parts,
    subgraph_name: String,
    context: Context,
    id: SubgraphRequestId,
}

/// Convert from Router Core JsonRequest to Router SubgraphRequest
pub(crate) async fn core_json_request_to_subgraph_request(
    mut core_request: CoreJsonRequest,
) -> Result<SubgraphRequest, BoxError> {
    // Extract context from extensions if present, otherwise create new
    let arc_metadata = core_request
        .extensions
        .remove::<Arc<RequestMetadata>>()
        .expect("subgraph metatada must exist");

    // There will be exactly one reference to SubgraphRequestMetadata. It's a private type no-one else can get it.
    let metadata =
        Arc::try_unwrap(arc_metadata).expect("there must be one reference to subgraph metadata");
    // Try to deserialize the JSON body as a GraphQL request
    let graphql_request = serde_json::from_value::<graphql::Request>(core_request.body)?;

    // Create a basic HTTP request wrapper for the GraphQL request
    let mut subgraph_request = http::Request::from_parts(metadata.http_parts, graphql_request);

    // Move our extensions into the new request
    *subgraph_request.extensions_mut() = core_request.extensions.into();

    Ok(SubgraphRequest {
        supergraph_request: metadata.supergraph_request,
        subgraph_request,
        operation_kind: metadata.operation_kind,
        context: metadata.context,
        subgraph_name: metadata.subgraph_name,
        subscription_stream: metadata.subscription_stream,
        connection_closed_signal: metadata.connection_closed_signal,
        query_hash: metadata.query_hash,
        authorization: metadata.authorization,
        executable_document: metadata.executable_document,
        id: metadata.id,
    })
}

/// Convert from Router SubgraphRequest to Router Core JsonRequest
pub(crate) async fn subgraph_request_to_core_json_request(
    mut subgraph_request: SubgraphRequest,
) -> Result<CoreJsonRequest, BoxError> {
    // Extract the extensions so that we can store the data in it.
    let mut extensions = std::mem::take(subgraph_request.subgraph_request.extensions_mut());

    // Deconstruct the SubgraphRequest into parts without cloning
    let http_request = subgraph_request.subgraph_request;
    // Deconstruct HTTP request into parts
    let (http_parts, body) = http_request.into_parts();

    // Serialize the GraphQL request body to JSON
    let json_body = serde_json::to_value(body)?;

    // Create subgraph metadata from the deconstructed parts
    let metadata = RequestMetadata {
        http_parts,
        supergraph_request: subgraph_request.supergraph_request,
        operation_kind: subgraph_request.operation_kind,
        context: subgraph_request.context,
        subgraph_name: subgraph_request.subgraph_name,
        subscription_stream: subgraph_request.subscription_stream,
        connection_closed_signal: subgraph_request.connection_closed_signal,
        query_hash: subgraph_request.query_hash,
        authorization: subgraph_request.authorization,
        executable_document: subgraph_request.executable_document,
        id: subgraph_request.id,
    };

    // Store subgraph metadata as an Arc
    extensions.insert(Arc::new(metadata));

    Ok(CoreJsonRequest {
        extensions: extensions.into(),
        body: json_body,
    })
}

/// Convert from Router Core JsonResponse to Router SubgraphResponse
pub(crate) async fn core_json_response_to_subgraph_response(
    mut core_response: CoreJsonResponse,
) -> Result<SubgraphResponse, BoxError> {
    // Extract subgraph response metadata from extensions
    let arc_metadata = core_response
        .extensions
        .remove::<Arc<ResponseMetadata>>()
        .expect("SubgraphResponseMetadata must exist in extensions");

    // There will be exactly one reference to SubgraphResponseMetadata. It's a private type no-one else can get it.
    let metadata = Arc::try_unwrap(arc_metadata)
        .expect("there must be one reference to subgraph response metadata");

    // Extract the first response from the stream
    use futures::StreamExt;
    let first_response = core_response.responses.next().await.transpose()?;

    // Convert the JSON value back to a GraphQL response
    let graphql_response = if let Some(json_val) = first_response {
        serde_json::from_value::<graphql::Response>(json_val)?
    } else {
        graphql::Response::default()
    };

    // Create HTTP response wrapper
    let mut http_response = http::Response::from_parts(metadata.http_parts, graphql_response);

    // Move our extensions into the new response
    *http_response.extensions_mut() = core_response.extensions.into();

    Ok(SubgraphResponse {
        response: http_response,
        context: metadata.context,
        subgraph_name: metadata.subgraph_name,
        id: metadata.id,
    })
}

/// Convert from Router SubgraphResponse to Router Core JsonResponse
pub(crate) async fn subgraph_response_to_core_json_response(
    mut subgraph_response: SubgraphResponse,
) -> Result<CoreJsonResponse, BoxError> {
    // Extract the extensions so that we can store the data in it.
    let mut extensions = std::mem::take(subgraph_response.response.extensions_mut());

    let (parts, response_body) = subgraph_response.response.into_parts();

    // Serialize the GraphQL response to JSON
    let json_response = serde_json::to_value(&response_body)?;

    // Create subgraph response metadata from the deconstructed parts
    let metadata = ResponseMetadata {
        http_parts: parts,
        subgraph_name: subgraph_response.subgraph_name,
        context: subgraph_response.context,
        id: subgraph_response.id,
    };

    // Store subgraph response metadata as an Arc
    extensions.insert(Arc::new(metadata));

    // Create a stream with just this one response
    let response_stream = futures::stream::once(async move { Ok(json_response) });

    Ok(CoreJsonResponse {
        extensions: extensions.into(),
        responses: Box::pin(response_stream),
    })
}

#[cfg(test)]
mod tests;
