//! Tests for the diagnostics service module

use std::sync::Arc;

use axum::body::Body;
use http::Method;
use http::Request;
use http::StatusCode;
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::tempdir;
use tower::Service;

use super::*;

/// Helper function to create a test router with the diagnostics service
fn create_test_router() -> Router {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let output_directory = temp_dir.path().to_str().unwrap().to_string();

    create_router(
        output_directory,
        Arc::from("test: config"),
        Arc::new("type Query { test: String }".to_string()),
    )
}

/// Helper to make a request and get the response
async fn make_request(
    router: &mut Router,
    method: Method,
    uri: &str,
) -> (StatusCode, String, http::HeaderMap) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("Failed to build request");

    let response = Service::call(router, request)
        .await
        .expect("Request failed");

    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Failed to read body")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    (status, body, headers)
}

// ============================================================================
// Dashboard Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_dashboard_endpoint_returns_html() {
    let mut router = create_test_router();
    let (status, body, headers) = make_request(&mut router, Method::GET, "/").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<!DOCTYPE html>") || body.contains("<html"));
    assert!(headers
        .get(http::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/html"));
}

// ============================================================================
// System Info Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_system_info_endpoint() {
    let mut router = create_test_router();
    let (status, body, headers) = make_request(&mut router, Method::GET, "/system_info.txt").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("SYSTEM INFORMATION"));
    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8"
    );
}

// ============================================================================
// Router Config Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_router_config_endpoint() {
    let mut router = create_test_router();
    let (status, body, headers) =
        make_request(&mut router, Method::GET, "/router_config.yaml").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "test: config");
    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "text/yaml; charset=utf-8"
    );
}

// ============================================================================
// Supergraph Schema Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_supergraph_schema_endpoint() {
    let mut router = create_test_router();
    let (status, body, headers) =
        make_request(&mut router, Method::GET, "/supergraph.graphql").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "type Query { test: String }");
    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8"
    );
}

// ============================================================================
// Memory Status Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_memory_status_endpoint_returns_json() {
    let mut router = create_test_router();
    let (status, body, headers) =
        make_request(&mut router, Method::GET, "/memory/status").await;

    // Should be OK on supported platforms, or NOT_IMPLEMENTED on unsupported
    assert!(status == StatusCode::OK || status == StatusCode::NOT_IMPLEMENTED);

    // Response should be valid JSON
    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON: {}", body);

    if status == StatusCode::OK {
        let json = json.unwrap();
        assert!(json.get("status").is_some());
    }

    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
}

// ============================================================================
// Memory Start/Stop Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_memory_start_endpoint() {
    let mut router = create_test_router();
    let (status, body, _) = make_request(&mut router, Method::POST, "/memory/start").await;

    // Should accept POST and return valid JSON
    assert!(
        status == StatusCode::OK
            || status == StatusCode::INTERNAL_SERVER_ERROR
            || status == StatusCode::NOT_IMPLEMENTED
    );

    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON");
}

#[tokio::test]
async fn test_memory_stop_endpoint() {
    let mut router = create_test_router();
    let (status, body, _) = make_request(&mut router, Method::POST, "/memory/stop").await;

    // Should accept POST and return valid JSON
    assert!(
        status == StatusCode::OK
            || status == StatusCode::INTERNAL_SERVER_ERROR
            || status == StatusCode::NOT_IMPLEMENTED
    );

    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON");
}

#[tokio::test]
async fn test_memory_dump_endpoint() {
    let mut router = create_test_router();
    let (status, body, _) = make_request(&mut router, Method::POST, "/memory/dump").await;

    // Should accept POST and return valid JSON
    assert!(
        status == StatusCode::OK
            || status == StatusCode::INTERNAL_SERVER_ERROR
            || status == StatusCode::NOT_IMPLEMENTED
    );

    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON");
}

// ============================================================================
// Memory Dumps List Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_memory_list_dumps_endpoint() {
    let mut router = create_test_router();
    let (status, body, _) = make_request(&mut router, Method::GET, "/memory/dumps").await;

    assert!(status == StatusCode::OK || status == StatusCode::NOT_IMPLEMENTED);

    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON");
}

#[tokio::test]
async fn test_memory_clear_dumps_endpoint() {
    let mut router = create_test_router();
    let (status, body, _) = make_request(&mut router, Method::DELETE, "/memory/dumps").await;

    assert!(status == StatusCode::OK || status == StatusCode::NOT_IMPLEMENTED);

    let json: Result<Value, _> = serde_json::from_str(&body);
    assert!(json.is_ok(), "Response should be valid JSON");
}

// ============================================================================
// Memory Dump File Endpoint Tests (Path Parameters)
// ============================================================================

#[tokio::test]
async fn test_memory_download_dump_not_found() {
    let mut router = create_test_router();
    let (status, _, _) = make_request(
        &mut router,
        Method::GET,
        "/memory/dumps/nonexistent.prof",
    )
    .await;

    // Should return 404 or NOT_IMPLEMENTED
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn test_memory_download_dump_invalid_filename() {
    let mut router = create_test_router();
    // Try path traversal attack
    let (status, _, _) =
        make_request(&mut router, Method::GET, "/memory/dumps/../../../etc/passwd").await;

    // Should return 404 (security validation should reject)
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_memory_delete_dump_invalid_filename() {
    let mut router = create_test_router();
    // Try path traversal attack
    let (status, _, _) = make_request(
        &mut router,
        Method::DELETE,
        "/memory/dumps/../secret.txt",
    )
    .await;

    // Should return 404 (security validation should reject)
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ============================================================================
// Export Endpoint Tests
// ============================================================================

#[tokio::test]
async fn test_export_endpoint_returns_archive() {
    let mut router = create_test_router();
    let (status, body, headers) = make_request(&mut router, Method::GET, "/export").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty(), "Export should return data");

    // Should have gzip content type
    let content_type = headers.get(http::header::CONTENT_TYPE).unwrap();
    assert!(content_type.to_str().unwrap().contains("gzip"));

    // Should have content-disposition header with filename
    let content_disposition = headers.get(http::header::CONTENT_DISPOSITION);
    assert!(content_disposition.is_some());
    let disposition_value = content_disposition.unwrap().to_str().unwrap();
    assert!(disposition_value.contains("attachment"));
    assert!(disposition_value.contains("filename"));
    assert!(disposition_value.contains("router-diagnostics"));
}

// ============================================================================
// Fallback Handler Tests (Static Resources)
// ============================================================================

#[tokio::test]
async fn test_fallback_serves_javascript_resources() {
    let mut router = create_test_router();
    let (status, body, headers) =
        make_request(&mut router, Method::GET, "/backtrace-processor.js").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty());
    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "application/javascript; charset=utf-8"
    );
}

#[tokio::test]
async fn test_fallback_serves_css_resources() {
    let mut router = create_test_router();
    let (status, body, headers) = make_request(&mut router, Method::GET, "/styles.css").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty());
    assert_eq!(
        headers.get(http::header::CONTENT_TYPE).unwrap(),
        "text/css; charset=utf-8"
    );
}

#[tokio::test]
async fn test_fallback_returns_404_for_unknown_resources() {
    let mut router = create_test_router();
    let (status, _, _) = make_request(&mut router, Method::GET, "/unknown-file.js").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ============================================================================
// Invalid Routes and Error Handling Tests
// ============================================================================

#[tokio::test]
async fn test_invalid_route_returns_404() {
    let mut router = create_test_router();
    let (status, _, _) = make_request(&mut router, Method::GET, "/invalid/route").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_wrong_http_method_on_post_endpoint() {
    let mut router = create_test_router();
    // Try GET on a POST-only endpoint
    let (status, _, _) = make_request(&mut router, Method::GET, "/memory/start").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn test_wrong_http_method_on_get_endpoint() {
    let mut router = create_test_router();
    // Try POST on a GET-only endpoint
    let (status, _, _) = make_request(&mut router, Method::POST, "/system_info.txt").await;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

// ============================================================================
// Helper Function Tests
// ============================================================================

#[test]
fn test_result_to_response_success() {
    let result: Result<&str, String> = Ok("success");
    let response = result_to_response(result, "Error message");

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn test_result_to_response_error() {
    let result: Result<&str, String> = Err("test error".to_string());
    let response = result_to_response(result, "Operation failed");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn test_not_found_response() {
    let response = not_found_response("File not found");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// State and Configuration Tests
// ============================================================================

#[tokio::test]
async fn test_router_with_empty_config() {
    let temp_dir = tempdir().expect("Failed to create temp dir");
    let router = create_router(
        temp_dir.path().to_str().unwrap().to_string(),
        Arc::from(""),
        Arc::new(String::new()),
    );

    let mut router_service = router;
    let (status, body, _) =
        make_request(&mut router_service, Method::GET, "/router_config.yaml").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "");
}

// ============================================================================
// Response Body Validation Tests
// ============================================================================

#[tokio::test]
async fn test_json_responses_are_valid_json() {
    let mut router = create_test_router();
    let json_endpoints = vec![
        (Method::GET, "/memory/status"),
        (Method::GET, "/memory/dumps"),
        (Method::POST, "/memory/start"),
        (Method::POST, "/memory/stop"),
        (Method::POST, "/memory/dump"),
        (Method::DELETE, "/memory/dumps"),
    ];

    for (method, endpoint) in json_endpoints {
        let (status, body, _) = make_request(&mut router, method, endpoint).await;

        if status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR {
            let json: Result<Value, _> = serde_json::from_str(&body);
            assert!(
                json.is_ok(),
                "Endpoint {} should return valid JSON, got: {}",
                endpoint,
                body
            );
        }
    }
}