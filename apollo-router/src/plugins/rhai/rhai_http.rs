//! HTTP layer (http_service) types and helpers for Rhai.

use std::ops::ControlFlow;

use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::Request;
use http::Response;
use http::StatusCode;
use tower::BoxError;

use super::ErrorDetails;
use crate::graphql::Error;
use crate::services::http_layer;

/// Wrapper for HTTP layer request, exposed to Rhai scripts.
#[derive(Default)]
pub(crate) struct RhaiHttpRequest {
    pub(crate) method: http::Method,
    pub(crate) uri: http::Uri,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: String,
}

impl RhaiHttpRequest {
    pub(crate) fn from_http_request(req: http_layer::HttpRequest) -> Self {
        let (parts, body) = req.into_parts();
        let body = String::from_utf8_lossy(body.as_ref()).to_string();
        Self {
            method: parts.method.clone(),
            uri: parts.uri.clone(),
            headers: parts.headers.clone(),
            body,
        }
    }

    pub(crate) fn into_http_request(self) -> http_layer::HttpRequest {
        let mut req = Request::builder()
            .method(self.method)
            .uri(self.uri)
            .body(Bytes::from(self.body))
            .expect("valid HTTP request");
        *req.headers_mut() = self.headers;
        req
    }
}

/// Wrapper for HTTP layer response, exposed to Rhai scripts.
#[derive(Default)]
pub(crate) struct RhaiHttpResponse {
    pub(crate) status_code: StatusCode,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: String,
}

impl RhaiHttpResponse {
    pub(crate) fn from_http_response(res: http_layer::HttpResponse) -> Self {
        let (parts, body) = res.into_parts();
        let body = String::from_utf8_lossy(body.as_ref()).to_string();
        Self {
            status_code: parts.status,
            headers: parts.headers.clone(),
            body,
        }
    }

    pub(crate) fn into_http_response(self) -> http_layer::HttpResponse {
        let mut res = Response::builder()
            .status(self.status_code)
            .body(Bytes::from(self.body))
            .expect("valid HTTP response");
        *res.headers_mut() = self.headers;
        res
    }
}

/// Build an HTTP layer error response from Rhai error details.
/// Used when a map_request or map_response callback throws.
pub(super) fn request_failure(
    error_details: ErrorDetails,
) -> Result<ControlFlow<http_layer::HttpResponse, http_layer::HttpRequest>, BoxError> {
    let body_str = if let Some(graphql_body) = error_details.body {
        serde_json::to_string(&graphql_body)?
    } else {
        let err = Error::builder()
            .message(error_details.message.unwrap_or_default())
            .build();
        serde_json::to_string(&crate::graphql::Response::builder().errors(vec![err]).build())?
    };
    let res = Response::builder()
        .status(error_details.status)
        .header(CONTENT_TYPE, "application/json")
        .body(Bytes::from(body_str))?;
    Ok(ControlFlow::Break(res))
}

/// Build an HTTP layer error response for response-stage failures.
pub(super) fn response_failure(error_details: ErrorDetails) -> http_layer::HttpResponse {
    let body_str = if let Some(graphql_body) = error_details.body {
        serde_json::to_string(&graphql_body).unwrap_or_default()
    } else {
        let err = Error::builder()
            .message(error_details.message.unwrap_or_default())
            .build();
        serde_json::to_string(&crate::graphql::Response::builder().errors(vec![err]).build())
            .unwrap_or_default()
    };
    Response::builder()
        .status(error_details.status)
        .header(CONTENT_TYPE, "application/json")
        .body(Bytes::from(body_str))
        .expect("valid HTTP response")
}
