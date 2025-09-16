//! HTTP response builder utilities for diagnostics plugin
//!
//! This module provides utilities to build consistent HTTP responses
//! with standardized headers and error handling.

use http::HeaderValue;
use http::StatusCode;
use http::header;
use mime::Mime;
use serde::Serialize;

use super::DiagnosticsError;
use super::DiagnosticsResult;
use crate::Context;
use crate::services::router::Response;
use crate::services::router::body;

/// Cache control settings for different types of responses
#[derive(Debug, Clone)]
pub(super) enum CacheControl {
    /// No cache headers for dynamic content
    NoCache,
    /// Long-term cache for static resources (1 year)
    StaticResource,
}

impl CacheControl {
    /// Get the cache-control header value
    fn header_value(&self) -> &'static str {
        match self {
            Self::NoCache => "no-cache, no-store, must-revalidate",
            Self::StaticResource => "public, max-age=31536000",
        }
    }

    /// Get additional cache-related headers for no-cache responses
    fn additional_headers(&self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::NoCache => vec![("pragma", "no-cache"), ("expires", "0")],
            Self::StaticResource => vec![],
        }
    }
}

/// Builder for creating standardized HTTP responses
pub(super) struct ResponseBuilder;

impl ResponseBuilder {
    /// Create a JSON response with standard headers
    pub(super) fn json_response<T: Serialize>(
        status: StatusCode,
        data: &T,
        cache_control: CacheControl,
        context: Context,
    ) -> DiagnosticsResult<Response> {
        let body_bytes = serde_json::to_vec(data).map_err(DiagnosticsError::Json)?;

        let mut response_builder = http::Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, cache_control.header_value());

        // Add additional cache headers if needed
        for (name, value) in cache_control.additional_headers() {
            response_builder = response_builder.header(name, value);
        }

        Response::http_response_builder()
            .response(
                response_builder
                    .body(body::from_bytes(body_bytes))
                    .map_err(DiagnosticsError::Http)?,
            )
            .context(context)
            .build()
            .map_err(|e| DiagnosticsError::Internal(e.to_string()))
    }

    /// Create a text response with standard headers
    pub(super) fn text_response(
        status: StatusCode,
        content_type: Mime,
        body_text: String,
        cache_control: CacheControl,
        context: Context,
    ) -> DiagnosticsResult<Response> {
        let mut response_builder = http::Response::builder()
            .status(status)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type.as_ref()).expect("valid MIME type"),
            )
            .header(header::CACHE_CONTROL, cache_control.header_value());

        // Add additional cache headers if needed
        for (name, value) in cache_control.additional_headers() {
            response_builder = response_builder.header(name, value);
        }

        Response::http_response_builder()
            .response(
                response_builder
                    .body(body::from_bytes(body_text.into_bytes()))
                    .map_err(DiagnosticsError::Http)?,
            )
            .context(context)
            .build()
            .map_err(|e| DiagnosticsError::Internal(e.to_string()))
    }

    /// Create a binary response for file downloads
    pub(super) fn binary_response(
        status: StatusCode,
        content_type: &str,
        body_bytes: Vec<u8>,
        filename: Option<&str>,
        cache_control: CacheControl,
        context: Context,
    ) -> DiagnosticsResult<Response> {
        let mut response_builder = http::Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, body_bytes.len().to_string())
            .header(header::CACHE_CONTROL, cache_control.header_value());

        // Add content-disposition header if filename is provided
        if let Some(filename) = filename {
            response_builder = response_builder.header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            );
        }

        // Add additional cache headers if needed
        for (name, value) in cache_control.additional_headers() {
            response_builder = response_builder.header(name, value);
        }

        Response::http_response_builder()
            .response(
                response_builder
                    .body(body::from_bytes(body_bytes))
                    .map_err(DiagnosticsError::Http)?,
            )
            .context(context)
            .build()
            .map_err(|e| DiagnosticsError::Internal(e.to_string()))
    }

    /// Create an error response with standard format
    pub(super) fn error_response(
        status: StatusCode,
        message: &str,
        context: Context,
    ) -> DiagnosticsResult<Response> {
        let error_data = serde_json::json!({
            "error": message,
            "status": status.as_u16()
        });

        Self::json_response(status, &error_data, CacheControl::NoCache, context)
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use mime::TEXT_PLAIN_UTF_8;
    use serde_json::json;

    use super::*;
    use crate::services::router::Request;
    use crate::services::router::{self};

    fn create_test_request() -> Request {
        router::Request::fake_builder()
            .method(Method::GET)
            .uri(http::Uri::from_static("http://localhost/test"))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn test_json_response() {
        let request = create_test_request();
        let data = json!({"status": "ok", "message": "test"});

        let response = ResponseBuilder::json_response(
            StatusCode::OK,
            &data,
            CacheControl::NoCache,
            request.context.clone(),
        )
        .unwrap();

        let router_response = response.response;
        assert_eq!(router_response.status(), StatusCode::OK);
        assert_eq!(
            router_response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            router_response
                .headers()
                .get(header::CACHE_CONTROL)
                .unwrap(),
            "no-cache, no-store, must-revalidate"
        );
    }

    #[tokio::test]
    async fn test_text_response() {
        let request = create_test_request();

        let response = ResponseBuilder::text_response(
            StatusCode::OK,
            TEXT_PLAIN_UTF_8,
            "Hello, World!".to_string(),
            CacheControl::NoCache,
            request.context.clone(),
        )
        .unwrap();

        let router_response = response.response;
        assert_eq!(router_response.status(), StatusCode::OK);
        assert_eq!(
            router_response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn test_error_response() {
        let request = create_test_request();

        let response = ResponseBuilder::error_response(
            StatusCode::NOT_FOUND,
            "Resource not found",
            request.context.clone(),
        )
        .unwrap();

        let router_response = response.response;
        assert_eq!(router_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            router_response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn test_static_resource_cache() {
        let request = create_test_request();
        let data = json!({"content": "static"});

        let response = ResponseBuilder::json_response(
            StatusCode::OK,
            &data,
            CacheControl::StaticResource,
            request.context.clone(),
        )
        .unwrap();

        let router_response = response.response;
        assert_eq!(
            router_response
                .headers()
                .get(header::CACHE_CONTROL)
                .unwrap(),
            "public, max-age=31536000"
        );
        // Should not have pragma or expires headers for static resources
        assert!(router_response.headers().get("pragma").is_none());
        assert!(router_response.headers().get("expires").is_none());
    }
}
