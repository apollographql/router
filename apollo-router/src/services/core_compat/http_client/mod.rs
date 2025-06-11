use http_body_util::BodyExt;
use tower::BoxError;

/// Router core http_client types
pub(super) use apollo_router_core::services::http_client::{
    Request as CoreRequest, Response as CoreResponse,
};

use crate::Context;
/// Router http service types
use crate::services::http::{HttpRequest as RouterHttpRequest, HttpResponse as RouterHttpResponse};
use crate::services::router::body::RouterBody;

/// Convert from Router Core Request to Router HttpRequest
impl From<CoreRequest> for RouterHttpRequest {
    fn from(mut core_request: CoreRequest) -> Self {
        // Extract context from extensions if present, otherwise create new
        let context = core_request
            .extensions_mut()
            .remove::<Context>()
            .expect("context must be set");

        // Take ownership of all remaining extensions (no cloning)
        let extensions = std::mem::take(core_request.extensions_mut());

        let (parts, core_body) = core_request.into_parts();

        // Map the body error type from BoxError to AxumError for RouterBody
        let router_body: RouterBody = core_body.map_err(axum::Error::new).boxed_unsync();

        let mut http_request = http::Request::from_parts(parts, router_body);

        // Move extensions to router request (no cloning)
        *http_request.extensions_mut() = extensions;

        Self {
            http_request,
            context,
        }
    }
}

/// Convert from Router HttpRequest to Router Core Request  
impl From<RouterHttpRequest> for CoreRequest {
    fn from(mut router_request: RouterHttpRequest) -> Self {
        // Take ownership of HTTP extensions from router request (no cloning)
        let mut extensions = std::mem::take(router_request.http_request.extensions_mut());

        let (parts, router_body) = router_request.http_request.into_parts();

        // Map the body error type from AxumError to BoxError
        let core_body = router_body
            .map_err(|err| -> BoxError { err.into() })
            .boxed_unsync();

        let mut core_request = http::Request::from_parts(parts, core_body);

        // Add the router context to extensions
        extensions.insert(router_request.context);

        // Move extensions to core request (no cloning)
        *core_request.extensions_mut() = extensions;

        core_request
    }
}

/// Convert from Router Core Response to Router HttpResponse
impl From<CoreResponse> for RouterHttpResponse {
    fn from(mut core_response: CoreResponse) -> Self {
        // Extract context from extensions if present, otherwise create new
        let context = core_response
            .extensions_mut()
            .remove::<Context>()
            .expect("context must exist");

        // Take ownership of all remaining extensions (no cloning)
        let extensions = std::mem::take(core_response.extensions_mut());

        let (parts, core_body) = core_response.into_parts();

        // Map the body error type from BoxError to AxumError for RouterBody
        let router_body: RouterBody = core_body.map_err(axum::Error::new).boxed_unsync();

        let mut http_response = http::Response::from_parts(parts, router_body);

        // Move extensions to router response (no cloning)
        *http_response.extensions_mut() = extensions;

        Self {
            http_response,
            context,
        }
    }
}

/// Convert from Router HttpResponse to Router Core Response
impl From<RouterHttpResponse> for CoreResponse {
    fn from(mut router_response: RouterHttpResponse) -> Self {
        // Take ownership of HTTP extensions from router response (no cloning)
        let mut extensions = std::mem::take(router_response.http_response.extensions_mut());

        let (parts, router_body) = router_response.http_response.into_parts();

        // Map the body error type from AxumError to BoxError
        let core_body = router_body
            .map_err(|err| -> BoxError { err.into() })
            .boxed_unsync();

        let mut core_response = http::Response::from_parts(parts, core_body);

        // Add the router context to extensions
        extensions.insert(router_response.context);

        // Move extensions to core response (no cloning)
        *core_response.extensions_mut() = extensions;

        core_response
    }
}

#[cfg(test)]
mod tests;
