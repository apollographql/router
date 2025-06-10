use super::*;
use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, Full};
use std::time::Duration;
use tower::{BoxError, ServiceExt};

/// Helper to create a test HTTP request
fn create_test_request(method: &str, uri: &str, body: &str) -> Request {
    let body = UnsyncBoxBody::new(Full::new(Bytes::from(body.to_string())).map_err(|_| unreachable!()));
    http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn test_reqwest_service_creation() {
    let service = ReqwestService::new();
    assert!(service.client.get("http://example.com").build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_with_custom_client() {
    let custom_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    
    let service = ReqwestService::with_client(custom_client);
    assert!(service.client.get("http://example.com").build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_default() {
    let service = ReqwestService::default();
    assert!(service.client.get("http://example.com").build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_tower_service_trait() {
    use tower::Service;
    
    let mut service = ReqwestService::new();
    
    // Test that the service is ready
    assert!(service.ready().await.is_ok());
    
    // The service should implement the Service trait
    fn _assert_service_trait<T>()
    where
        T: Service<Request, Response = Response, Error = Error> + Clone,
    {
        // This function exists only to verify trait bounds at compile time
    }
    
    _assert_service_trait::<ReqwestService>();
}

#[tokio::test]
async fn test_error_into_box_error() {
    let error = Error::InvalidRequest {
        details: "test error".to_string(),
    };
    
    let _box_error: BoxError = error.into();
    // Test passes if compilation succeeds
}

#[tokio::test]
async fn test_reqwest_service_network_error() {
    let service = ReqwestService::new();
    
    // Create a request with an invalid URL that will definitely fail
    let request = create_test_request("POST", "http://invalid-domain-12345.invalid", "test body");
    
    // This test verifies that the service handles network errors gracefully
    let result = service.execute_request(request).await;
    
    // The request should fail with a network error
    assert!(result.is_err());
    
    // Verify it's the right type of error
    if let Err(Error::RequestFailed { .. }) = result {
        // This is the expected error type
    } else {
        panic!("Expected RequestFailed error variant");
    }
}

// Note: Network-dependent tests are not included as they require external dependencies
// and would make tests flaky. In a real-world scenario, you would typically:
// 1. Use a test server (like wiremock or httpmock) for integration tests
// 2. Use dependency injection to mock the reqwest::Client for unit tests
// 3. Create integration tests that use real HTTP endpoints in a controlled environment

#[tokio::test]
async fn test_error_types_debug_and_display() {
    // For testing, we'll create errors using the actual enum variants
    // without trying to create a real reqwest::Error from io::Error
    
    let invalid_request_error = Error::InvalidRequest {
        details: "Test details".to_string(),
    };
    
    let response_processing_error = Error::ResponseProcessingFailed {
        source: Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Test error")),
        context: "Test context".to_string(),
    };
    
    // Test that errors can be formatted
    assert!(!format!("{:?}", invalid_request_error).is_empty());
    assert!(!format!("{}", invalid_request_error).is_empty());
    
    assert!(!format!("{:?}", response_processing_error).is_empty());
    assert!(!format!("{}", response_processing_error).is_empty());
    
    // Test that error can be converted to BoxError
    let _box_error: BoxError = invalid_request_error.into();
    let _box_error2: BoxError = response_processing_error.into();
} 