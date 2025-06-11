use super::*;
use apollo_router_core::Extensions;
use bytes::Bytes;
use http::StatusCode;
use http_body_util::{Full, combinators::UnsyncBoxBody};

use crate::Context;
use crate::services::router::body;

#[tokio::test]
async fn test_router_to_core_http_request_conversion() {
    // Create a test router Context with some data
    let context = {
        let mut context = Context::new();
        assert!(context.insert("test_key", "test_value".to_string()).is_ok());
        context
    };
    
    // Create a router HTTP request
    let body = body::from_bytes("test request body");
    let http_request = http::Request::builder()
        .method("POST")
        .uri("http://example.com/graphql")
        .header("content-type", "application/json")
        .header("authorization", "Bearer token123")
        .body(body)
        .unwrap();
    
    let router_request = RouterHttpRequest { http_request, context: context.clone() };
    
    // Convert to core request
    let core_request = router_to_core_http_request(router_request)
        .await
        .expect("should convert router request to core request");
    
    // Verify HTTP parts are preserved
    assert_eq!(core_request.method(), "POST");
    assert_eq!(core_request.uri(), "http://example.com/graphql");
    assert_eq!(core_request.headers().get("content-type").unwrap(), "application/json");
    assert_eq!(core_request.headers().get("authorization").unwrap(), "Bearer token123");
    
    // Verify body was converted
    let (parts, body) = core_request.into_parts();
    let body_bytes = body.collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes, "test request body");
    
    // Verify context was stored in extensions
    let core_extensions: Extensions = parts.extensions.into();
    let extracted_context = core_extensions.get::<Context>().unwrap();
    let test_value: String = extracted_context.get("test_key").unwrap().unwrap();
    assert_eq!(test_value, "test_value");
}

#[tokio::test]
async fn test_core_to_router_http_response_conversion() {
    // Create a test router Context
    let context = {
        let mut context = Context::new();
        assert!(context.insert("response_key", 42i32).is_ok());
        context
    };
    
    // Create core Extensions with the context
    let mut core_extensions = Extensions::new();
    core_extensions.insert(context.clone());
    
    // Create a core HTTP response
    let body = UnsyncBoxBody::new(
        Full::new(Bytes::from("test response body"))
            .map_err(|e: std::convert::Infallible| -> BoxError { match e {} })
    );
    
    let mut core_response = http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header("cache-control", "no-cache")
        .body(body)
        .unwrap();
    
    // Store core Extensions in http::Extensions
    let http_extensions: http::Extensions = core_extensions.into();
    *core_response.extensions_mut() = http_extensions;
    
    // Convert to router response
    let router_response = core_to_router_http_response(core_response)
        .await
        .expect("should convert core response to router response");
    
    // Verify HTTP parts are preserved
    assert_eq!(router_response.http_response.status(), StatusCode::OK);
    assert_eq!(router_response.http_response.headers().get("content-type").unwrap(), "application/json");
    assert_eq!(router_response.http_response.headers().get("cache-control").unwrap(), "no-cache");
    
    // Verify body was converted
    let body_string = body::into_string(router_response.http_response.into_body()).await.unwrap();
    assert_eq!(body_string, "test response body");
    
    // Verify context was extracted properly
    let response_value: i32 = router_response.context.get("response_key").unwrap().unwrap();
    assert_eq!(response_value, 42);
}

#[tokio::test]
async fn test_round_trip_conversion() {
    // Create original router request
    let original_context = {
        let mut original_context = Context::new();
        assert!(original_context.insert("round_trip_test", true).is_ok());
        original_context
    };
    
    let body = body::from_bytes("round trip test body");
    let http_request = http::Request::builder()
        .method("PUT")
        .uri("http://example.com/api")
        .header("x-test-header", "test-value")
        .body(body)
        .unwrap();
    
    let original_router_request = RouterHttpRequest {
        http_request,
        context: original_context.clone(),
    };
    
    // Convert router -> core -> router
    let core_request = router_to_core_http_request(original_router_request)
        .await
        .expect("router to core conversion should succeed");
    
    let converted_router_request = core_to_router_http_request(core_request)
        .await
        .expect("core to router conversion should succeed");
    
    // Verify everything was preserved
    assert_eq!(converted_router_request.http_request.method(), "PUT");
    assert_eq!(converted_router_request.http_request.uri(), "http://example.com/api");
    assert_eq!(
        converted_router_request.http_request.headers().get("x-test-header").unwrap(),
        "test-value"
    );
    
    // Verify body
    let body_string = body::into_string(converted_router_request.http_request.into_body())
        .await
        .unwrap();
    assert_eq!(body_string, "round trip test body");
    
    // Verify context
    let round_trip_value: bool = converted_router_request
        .context
        .get("round_trip_test")
        .unwrap()
        .unwrap();
    assert_eq!(round_trip_value, true);
}

#[tokio::test]
async fn test_core_to_router_request_with_no_context() {
    // Create a core request without context in extensions
    let body = UnsyncBoxBody::new(
        Full::new(Bytes::from("no context test"))
            .map_err(|e: std::convert::Infallible| -> BoxError { match e {} })
    );
    
    let core_request = http::Request::builder()
        .method("GET")
        .uri("http://example.com")
        .body(body)
        .unwrap();
    
    // Convert to router request - should create default context
    let router_request = core_to_router_http_request(core_request)
        .await
        .expect("should convert core request to router request");
    
    // Verify request is preserved
    assert_eq!(router_request.http_request.method(), "GET");
    assert_eq!(router_request.http_request.uri(), "http://example.com");
    
    // Verify default context was created
    assert!(!router_request.context.id.is_empty());
}

#[tokio::test]
async fn test_error_on_missing_context_in_response() {
    // Create a core response without context in extensions
    let body = UnsyncBoxBody::new(
        Full::new(Bytes::from("error test"))
            .map_err(|e: std::convert::Infallible| -> BoxError { match e {} })
    );
    
    let core_response = http::Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(body)
        .unwrap();
    
    // Conversion should fail due to missing context
    let result = core_to_router_http_response(core_response).await;
    assert!(result.is_err());
    
    if let Err(ConversionError::ContextExtractionFailed) = result {
        // Expected error
    } else {
        panic!("Expected ContextExtractionFailed error");
    }
} 