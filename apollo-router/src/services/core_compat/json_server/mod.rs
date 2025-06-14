use std::sync::Arc;

/// Router core json_server types
pub(super) use apollo_router_core::services::json_server::Request as CoreJsonRequest;
/// Router core json_server types
pub(super) use apollo_router_core::services::json_server::Response as CoreJsonResponse;
use futures::StreamExt;
use tower::BoxError;

use super::RequestMetadata;
use super::ResponseMetadata;
use crate::graphql;
use crate::services::supergraph::Request as SupergraphRequest;
use crate::services::supergraph::Response as SupergraphResponse;

/// Convert from Router Core json_server Request to Router SupergraphRequest
pub(crate) fn core_json_request_to_supergraph_request(
    mut core_request: CoreJsonRequest,
) -> Result<SupergraphRequest, BoxError> {
    // Extract request metadata from extensions
    let arc_metadata = core_request
        .extensions
        .remove::<Arc<RequestMetadata>>()
        .expect("RequestMetadata must exist in extensions");

    // There will be exactly one reference to RequestMetadata. It's a private type no-one else can get it.
    let metadata =
        Arc::try_unwrap(arc_metadata).expect("there must be one reference to request metadata");

    // Take ownership of all remaining extensions (no cloning)
    let extensions = std::mem::take(&mut core_request.extensions);

    // Convert JSON value to GraphQL request
    let graphql_request = serde_json::from_value::<graphql::Request>(core_request.body)?;

    // Create HTTP request with GraphQL body
    let mut supergraph_request = http::Request::from_parts(metadata.http_parts, graphql_request);

    // Move extensions to supergraph request (no cloning)
    *supergraph_request.extensions_mut() = extensions.into();

    Ok(SupergraphRequest {
        supergraph_request,
        context: metadata.context,
    })
}

/// Convert from Router SupergraphRequest to Router Core json_server Request
pub(crate) fn supergraph_request_to_core_json_request(
    mut supergraph_request: SupergraphRequest,
) -> Result<CoreJsonRequest, BoxError> {
    // Take ownership of HTTP extensions from supergraph request (no cloning)
    let mut extensions = std::mem::take(supergraph_request.supergraph_request.extensions_mut());

    let (parts, graphql_request) = supergraph_request.supergraph_request.into_parts();

    // Convert GraphQL request to JSON value
    let json_body = serde_json::to_value(graphql_request)?;

    // Create request metadata from the supergraph parts
    let metadata = RequestMetadata {
        http_parts: parts,
        context: supergraph_request.context,
    };

    // Store request metadata as an Arc
    extensions.insert(Arc::new(metadata));

    Ok(CoreJsonRequest {
        extensions: extensions.into(),
        body: json_body,
    })
}

/// Convert from Router Core json_server Response to Router SupergraphResponse
pub(crate) fn core_json_response_to_supergraph_response(
    mut core_response: CoreJsonResponse,
) -> Result<SupergraphResponse, BoxError> {
    // Extract response metadata from extensions
    let arc_metadata = core_response
        .extensions
        .remove::<Arc<ResponseMetadata>>()
        .expect("ResponseMetadata must exist in extensions");

    // There will be exactly one reference to ResponseMetadata. It's a private type no-one else can get it.
    let metadata =
        Arc::try_unwrap(arc_metadata).expect("there must be one reference to response metadata");

    // Take ownership of all remaining extensions (no cloning)
    let extensions = std::mem::take(&mut core_response.extensions);

    // Convert JSON stream to GraphQL response stream
    // Note: We filter out errors and log them, keeping only successful conversions
    let graphql_stream = core_response
        .responses
        .filter_map(|result| async move {
            match result {
                Ok(json_value) => match serde_json::from_value::<graphql::Response>(json_value) {
                    Ok(graphql_response) => Some(graphql_response),
                    Err(err) => {
                        tracing::error!("Failed to convert JSON to GraphQL response: {}", err);
                        None
                    }
                },
                Err(err) => {
                    tracing::error!("Error in JSON response stream: {}", err);
                    None
                }
            }
        })
        .boxed();

    // Create HTTP response with GraphQL stream body
    let mut http_response = http::Response::from_parts(metadata.http_parts, graphql_stream);

    // Move extensions to supergraph response (no cloning)
    *http_response.extensions_mut() = extensions.into();

    Ok(SupergraphResponse {
        response: http_response,
        context: metadata.context,
    })
}

/// Convert from Router SupergraphResponse to Router Core json_server Response
pub(crate) fn supergraph_response_to_core_json_response(
    mut supergraph_response: SupergraphResponse,
) -> Result<CoreJsonResponse, BoxError> {
    // Take ownership of HTTP extensions from supergraph response (no cloning)
    let mut extensions = std::mem::take(supergraph_response.response.extensions_mut());

    let (parts, graphql_stream) = supergraph_response.response.into_parts();

    // Convert GraphQL response stream to JSON stream
    let json_stream = graphql_stream
        .map(|graphql_response| {
            serde_json::to_value(&graphql_response).map_err(|err| -> BoxError { err.into() })
        })
        .boxed();

    // Create response metadata from the supergraph parts
    let metadata = ResponseMetadata {
        http_parts: parts,
        context: supergraph_response.context,
    };

    // Store response metadata as an Arc
    extensions.insert(Arc::new(metadata));

    Ok(CoreJsonResponse {
        extensions: extensions.into(),
        responses: json_stream,
    })
}

#[cfg(test)]
mod tests;
