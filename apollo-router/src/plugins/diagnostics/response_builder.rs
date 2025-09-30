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
    use http_body_util::BodyExt;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn test_json_response_ok_status() {
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
        assert_eq!(response.headers().get("pragma").unwrap(), "no-cache");
        assert_eq!(response.headers().get("expires").unwrap(), "0");
    }

    #[tokio::test]
    async fn test_json_response_body_content() {
        let data = json!({"key": "value", "number": 42});

        let response =
            ResponseBuilder::json_response(StatusCode::OK, &data, CacheControl::NoCache).unwrap();

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(body_json["key"], "value");
        assert_eq!(body_json["number"], 42);
    }

    #[tokio::test]
    async fn test_binary_response_with_filename() {
        let data = b"binary data content".to_vec();

        let response = ResponseBuilder::binary_response(
            StatusCode::OK,
            "application/octet-stream",
            data.clone(),
            Some("test.bin"),
            CacheControl::NoCache,
        )
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            &data.len().to_string()
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"test.bin\""
        );
    }

    #[tokio::test]
    async fn test_binary_response_without_filename() {
        let data = b"some data".to_vec();

        let response = ResponseBuilder::binary_response(
            StatusCode::OK,
            "application/octet-stream",
            data.clone(),
            None,
            CacheControl::NoCache,
        )
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::CONTENT_DISPOSITION).is_none(),
            "Content-Disposition header should not be present when filename is None"
        );
    }

    #[tokio::test]
    async fn test_binary_response_body_content() {
        let original_data = b"This is binary content with \x00 null bytes".to_vec();

        let response = ResponseBuilder::binary_response(
            StatusCode::OK,
            "application/octet-stream",
            original_data.clone(),
            Some("test.bin"),
            CacheControl::NoCache,
        )
        .unwrap();

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(
            body_bytes.as_ref(),
            original_data.as_slice(),
            "Response body should exactly match input data"
        );
    }

    #[tokio::test]
    async fn test_binary_response_empty_data() {
        let data = Vec::new();

        let response = ResponseBuilder::binary_response(
            StatusCode::OK,
            "application/octet-stream",
            data,
            Some("empty.bin"),
            CacheControl::NoCache,
        )
        .unwrap();

        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "0");

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body_bytes.is_empty());
    }
}
