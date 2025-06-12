use http_body_util::BodyExt;
use std::sync::Arc;
use tower::BoxError;

/// Router core http_server types
pub(super) use apollo_router_core::services::http_server::{
    Request as CoreRequest, Response as CoreResponse,
};

use crate::Context;
use crate::services::router::body::RouterBody;
use crate::services::router::{Request as RouterRequest, Response as RouterResponse};

/// Metadata for storing router request information in extensions during conversion
#[derive(Debug)]
struct RequestMetadata {
    context: Context,
}

/// Metadata for storing router response information in extensions during conversion
#[derive(Debug)]
struct ResponseMetadata {
    context: Context,
}

/// Convert from Router Core http_server Request to Router Request
pub(crate) fn core_request_to_router_request(
    mut core_request: CoreRequest,
) -> Result<RouterRequest, BoxError> {
    // Extract request metadata from extensions
    let arc_metadata = core_request
        .extensions_mut()
        .remove::<Arc<RequestMetadata>>()
        .expect("RequestMetadata must exist in extensions");

    // There will be exactly one reference to RequestMetadata. It's a private type no-one else can get it.
    let metadata =
        Arc::try_unwrap(arc_metadata).expect("there must be one reference to request metadata");

    // Take ownership of all remaining extensions (no cloning)
    let extensions = std::mem::take(core_request.extensions_mut());

    let (parts, core_body) = core_request.into_parts();

    // Map the body error type from BoxError to AxumError for RouterBody
    let router_body: RouterBody = core_body.map_err(axum::Error::new).boxed_unsync();

    let mut router_request = http::Request::from_parts(parts, router_body);

    // Move extensions to router request (no cloning)
    *router_request.extensions_mut() = extensions;

    Ok(RouterRequest {
        router_request,
        context: metadata.context,
    })
}

/// Convert from Router Request to Router Core http_server Request
pub(crate) fn router_request_to_core_request(
    mut router_request: RouterRequest,
) -> Result<CoreRequest, BoxError> {
    // Take ownership of HTTP extensions from router request (no cloning)
    let mut extensions = std::mem::take(router_request.router_request.extensions_mut());

    let (parts, router_body) = router_request.router_request.into_parts();

    // Map the body error type from AxumError to BoxError
    let core_body = router_body
        .map_err(|err| -> BoxError { err.into() })
        .boxed_unsync();

    let mut core_request = http::Request::from_parts(parts, core_body);

    // Create request metadata from the router context
    let metadata = RequestMetadata {
        context: router_request.context,
    };

    // Store request metadata as an Arc
    extensions.insert(Arc::new(metadata));

    // Move extensions to core request (no cloning)
    *core_request.extensions_mut() = extensions;

    Ok(core_request)
}

/// Convert from Router Core http_server Response to Router Response
pub(crate) fn core_response_to_router_response(
    mut core_response: CoreResponse,
) -> Result<RouterResponse, BoxError> {
    // Extract response metadata from extensions
    let arc_metadata = core_response
        .extensions_mut()
        .remove::<Arc<ResponseMetadata>>()
        .expect("ResponseMetadata must exist in extensions");

    // There will be exactly one reference to ResponseMetadata. It's a private type no-one else can get it.
    let metadata =
        Arc::try_unwrap(arc_metadata).expect("there must be one reference to response metadata");

    // Take ownership of all remaining extensions (no cloning)
    let extensions = std::mem::take(core_response.extensions_mut());

    let (parts, core_body) = core_response.into_parts();

    // Map the body error type from BoxError to AxumError for RouterBody
    let router_body: RouterBody = core_body.map_err(axum::Error::new).boxed_unsync();

    let mut http_response = http::Response::from_parts(parts, router_body);

    // Move extensions to router response (no cloning)
    *http_response.extensions_mut() = extensions;

    Ok(RouterResponse {
        response: http_response,
        context: metadata.context,
    })
}

/// Convert from Router Response to Router Core http_server Response
pub(crate) fn router_response_to_core_response(
    mut router_response: RouterResponse,
) -> Result<CoreResponse, BoxError> {
    // Take ownership of HTTP extensions from router response (no cloning)
    let mut extensions = std::mem::take(router_response.response.extensions_mut());

    let (parts, router_body) = router_response.response.into_parts();

    // Map the body error type from AxumError to BoxError
    let core_body = router_body
        .map_err(|err| -> BoxError { err.into() })
        .boxed_unsync();

    let mut core_response = http::Response::from_parts(parts, core_body);

    // Create response metadata from the router context
    let metadata = ResponseMetadata {
        context: router_response.context,
    };

    // Store response metadata as an Arc
    extensions.insert(Arc::new(metadata));

    // Move extensions to core response (no cloning)
    *core_response.extensions_mut() = extensions;

    Ok(core_response)
}

#[cfg(test)]
mod tests;
