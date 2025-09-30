//! HTTP response builder utilities for diagnostics plugin
//!
//! This module provides utilities to build consistent HTTP responses
//! with standardized headers and error handling.

use axum::body::Body;
use http::Response;
use http::StatusCode;
use http::header;
use serde::Serialize;

use super::DiagnosticsError;
use super::DiagnosticsResult;

/// Cache control settings for different types of responses
#[derive(Debug, Clone)]
pub(super) enum CacheControl {
    /// No cache headers for dynamic content
    NoCache,
}

impl CacheControl {
    /// Get the cache-control header value
    fn header_value(&self) -> &'static str {
        match self {
            Self::NoCache => "no-cache, no-store, must-revalidate",
        }
    }

    /// Get additional cache-related headers for no-cache responses
    fn additional_headers(&self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::NoCache => vec![("pragma", "no-cache"), ("expires", "0")],
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
    ) -> DiagnosticsResult<Response<Body>> {
        let body_bytes = serde_json::to_vec(data).map_err(DiagnosticsError::Json)?;

        let mut response_builder = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, cache_control.header_value());

        // Add additional cache headers if needed
        for (name, value) in cache_control.additional_headers() {
            response_builder = response_builder.header(name, value);
        }

        response_builder
            .body(Body::from(body_bytes))
            .map_err(DiagnosticsError::Http)
    }

    /// Create a binary response for file downloads
    pub(super) fn binary_response(
        status: StatusCode,
        content_type: &str,
        body_bytes: Vec<u8>,
        filename: Option<&str>,
        cache_control: CacheControl,
    ) -> DiagnosticsResult<Response<Body>> {
        let mut response_builder = Response::builder()
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

        response_builder
            .body(Body::from(body_bytes))
            .map_err(DiagnosticsError::Http)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_json_response() {
        let data = json!({"status": "ok", "message": "test"});

        let response =
            ResponseBuilder::json_response(StatusCode::OK, &data, CacheControl::NoCache).unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache, no-store, must-revalidate"
        );
    }
}
