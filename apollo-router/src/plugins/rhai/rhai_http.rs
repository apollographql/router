//! HTTP layer (http_service) and service HTTP (service_http) types and helpers for Rhai.

use std::ops::ControlFlow;

use bytes::Bytes;
use http::Request;
use http::Response;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use tower::BoxError;

use super::ErrorDetails;
use crate::graphql::Error;
use crate::services::http as service_http;
use crate::services::http_layer;
use crate::services::router;

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

    pub(crate) fn into_http_request(self) -> Result<http_layer::HttpRequest, BoxError> {
        let mut req = Request::builder()
            .method(self.method)
            .uri(self.uri)
            .body(Bytes::from(self.body))?;
        *req.headers_mut() = self.headers;
        Ok(req)
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

    pub(crate) fn into_http_response(self) -> Result<http_layer::HttpResponse, BoxError> {
        let mut res = Response::builder()
            .status(self.status_code)
            .body(Bytes::from(self.body))?;
        *res.headers_mut() = self.headers;
        Ok(res)
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
        serde_json::to_string(
            &crate::graphql::Response::builder()
                .errors(vec![err])
                .build(),
        )?
    };
    let res = Response::builder()
        .status(error_details.status)
        .header(CONTENT_TYPE, "application/json")
        .body(Bytes::from(body_str))?;
    Ok(ControlFlow::Break(res))
}

/// Wrapper for service HTTP request (service_http), exposed to Rhai scripts.
#[derive(Default)]
pub(crate) struct RhaiServiceHttpRequest {
    pub(crate) method: http::Method,
    pub(crate) uri: http::Uri,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: String,
    /// Service name (subgraph/connector) when available.
    pub(crate) service_name: Option<String>,
}

impl RhaiServiceHttpRequest {
    pub(crate) fn from_parts(
        parts: http::request::Parts,
        body: Bytes,
        _context: &crate::Context,
        service_name: Option<String>,
    ) -> Self {
        Self {
            method: parts.method.clone(),
            uri: parts.uri.clone(),
            headers: parts.headers.clone(),
            body: String::from_utf8_lossy(&body).to_string(),
            service_name,
        }
    }

    pub(crate) fn into_http_request(
        self,
        context: crate::Context,
    ) -> Result<service_http::HttpRequest, BoxError> {
        let mut req = Request::builder()
            .method(self.method)
            .uri(self.uri)
            .body(router::body::from_bytes(Bytes::from(self.body)))?;
        *req.headers_mut() = self.headers;
        Ok(service_http::HttpRequest {
            http_request: req,
            context,
        })
    }
}

/// Wrapper for service HTTP response (service_http), exposed to Rhai scripts.
#[derive(Default)]
pub(crate) struct RhaiServiceHttpResponse {
    pub(crate) status_code: StatusCode,
    pub(crate) headers: http::HeaderMap,
    pub(crate) body: String,
}

impl RhaiServiceHttpResponse {
    pub(crate) fn from_parts(parts: http::response::Parts, body: Bytes) -> Self {
        Self {
            status_code: parts.status,
            headers: parts.headers.clone(),
            body: String::from_utf8_lossy(&body).to_string(),
        }
    }

    pub(crate) fn into_http_response(
        self,
        context: crate::Context,
    ) -> Result<service_http::HttpResponse, BoxError> {
        let mut res = Response::builder()
            .status(self.status_code)
            .body(router::body::from_bytes(Bytes::from(self.body)))?;
        *res.headers_mut() = self.headers;
        Ok(service_http::HttpResponse {
            http_response: res,
            context,
        })
    }
}

/// Build an HTTP layer error response for response-stage failures.
pub(super) fn response_failure(error_details: ErrorDetails) -> http_layer::HttpResponse {
    let body_str = if let Some(graphql_body) = error_details.body {
        serde_json::to_string(&graphql_body).unwrap_or_else(|e| {
            tracing::error!("failed to serialize error response: {e}");
            r#"{"errors":[{"message":"Internal error"}]}"#.to_string()
        })
    } else {
        let err = Error::builder()
            .message(error_details.message.unwrap_or_default())
            .build();
        serde_json::to_string(
            &crate::graphql::Response::builder()
                .errors(vec![err])
                .build(),
        )
        .unwrap_or_else(|e| {
            tracing::error!("failed to serialize error response: {e}");
            r#"{"errors":[{"message":"Internal error"}]}"#.to_string()
        })
    };
    Response::builder()
        .status(error_details.status)
        .header(CONTENT_TYPE, "application/json")
        .body(Bytes::from(body_str))
        .expect("valid HTTP response")
}

/// Build a service HTTP error response for request/response stage failures.
pub(super) fn service_http_response_failure(
    context: crate::Context,
    error_details: ErrorDetails,
) -> service_http::HttpResponse {
    let body_str = if let Some(graphql_body) = error_details.body {
        serde_json::to_string(&graphql_body).unwrap_or_else(|e| {
            tracing::error!("failed to serialize error response: {e}");
            r#"{"errors":[{"message":"Internal error"}]}"#.to_string()
        })
    } else {
        let err = Error::builder()
            .message(error_details.message.unwrap_or_default())
            .build();
        serde_json::to_string(
            &crate::graphql::Response::builder()
                .errors(vec![err])
                .build(),
        )
        .unwrap_or_else(|e| {
            tracing::error!("failed to serialize error response: {e}");
            r#"{"errors":[{"message":"Internal error"}]}"#.to_string()
        })
    };
    let http_response = Response::builder()
        .status(error_details.status)
        .header(CONTENT_TYPE, "application/json")
        .body(router::body::from_bytes(Bytes::from(body_str)))
        .expect("valid HTTP response");
    service_http::HttpResponse {
        http_response,
        context,
    }
}
