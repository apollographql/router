use crate::layers::bytes_to_json::BytesToJsonLayer;
use crate::services::json_server::{Request as JsonRequest, Response as JsonResponse};
use crate::services::bytes_server::{Request as BytesRequest};
use crate::json::JsonValue;
use bytes::Bytes;
use futures::stream;
use futures::StreamExt;
use serde_json::json;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_test::mock;

#[tokio::test]
async fn test_bytes_to_json_conversion() {
    let (mut mock_service, mut handle) = mock::pair::<JsonRequest, JsonResponse>();
    
    // Set up the mock to expect a JSON request and return a JSON response
    handle.allow(1);
    let _ = tokio::task::spawn(async move {
        let (request, response) = handle.next_request().await.expect("service must not fail");
        
        // Verify the JSON was parsed correctly
        let expected_json = json!({"query": "{ hello }", "variables": {}});
        assert_eq!(request.body, expected_json);
        
        // Send back a JSON response
        let response_json = json!({"data": {"hello": "world"}});
        response.send_response(JsonResponse {
            extensions: crate::Extensions::default(),
            responses: Box::pin(stream::once(async move { response_json })),
        });
    });

    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(BytesToJsonLayer)
        .service(mock_service);

    // Create a test bytes request with JSON content
    let json_bytes = serde_json::to_vec(&json!({"query": "{ hello }", "variables": {}}))
        .expect("JSON serialization should succeed");
    
    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(json_bytes),
    };

    // Call the service and verify the response
    let response = service
        .oneshot(bytes_req)
        .await
        .expect("response expected");

    // Collect the response stream
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item");

    // Parse the response back to JSON to verify it's correct
    let response_json: JsonValue = serde_json::from_slice(&response_bytes)
        .expect("Response should be valid JSON");
    
    let expected_response = json!({"data": {"hello": "world"}});
    assert_eq!(response_json, expected_response);
}

#[tokio::test]
async fn test_invalid_json_bytes() {
    let (mut mock_service, mut _handle) = mock::pair::<JsonRequest, JsonResponse>();
    
    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(BytesToJsonLayer)
        .service(mock_service);

    // Create a test bytes request with invalid JSON content
    let invalid_json_bytes = b"invalid json content";
    
    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(&invalid_json_bytes[..]),
    };

    // Call the service and expect an error
    let result = service.oneshot(bytes_req).await;
    assert!(result.is_err(), "Should return error for invalid JSON");
    
    if let Err(error) = result {
        let error_message = error.to_string();
        assert!(error_message.contains("Failed to parse JSON from bytes"));
    }
}

#[tokio::test]
async fn test_empty_json_object() {
    let (mut mock_service, mut handle) = mock::pair::<JsonRequest, JsonResponse>();
    
    // Set up the mock to expect an empty JSON object
    handle.allow(1);
    let _ = tokio::task::spawn(async move {
        let (request, response) = handle.next_request().await.expect("service must not fail");
        
        // Verify the empty JSON object was parsed correctly
        let expected_json = json!({});
        assert_eq!(request.body, expected_json);
        
        // Send back a JSON response
        let response_json = json!({"status": "ok"});
        response.send_response(JsonResponse {
            extensions: crate::Extensions::default(),
            responses: Box::pin(stream::once(async move { response_json })),
        });
    });

    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(BytesToJsonLayer)
        .service(mock_service);

    // Create a test bytes request with empty JSON object
    let json_bytes = b"{}";
    
    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(&json_bytes[..]),
    };

    // Call the service and verify the response
    let response = service
        .oneshot(bytes_req)
        .await
        .expect("response expected");

    // Collect the response stream
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item");

    // Parse the response back to JSON to verify it's correct
    let response_json: JsonValue = serde_json::from_slice(&response_bytes)
        .expect("Response should be valid JSON");
    
    let expected_response = json!({"status": "ok"});
    assert_eq!(response_json, expected_response);
}

#[tokio::test]
async fn test_extensions_passthrough() {
    let (mut mock_service, mut handle) = mock::pair::<JsonRequest, JsonResponse>();
    
    // Set up the mock to verify extensions are passed through
    handle.allow(1);
    let _ = tokio::task::spawn(async move {
        let (request, response) = handle.next_request().await.expect("service must not fail");
        
        // Verify the JSON was parsed correctly
        let expected_json = json!({"query": "{ hello }"});
        assert_eq!(request.body, expected_json);
        
        // Verify the extensions were extended from BytesRequest (parent values accessible)
        let test_value: Option<String> = request.extensions.get();
        assert_eq!(test_value, Some("test_context".to_string()));
        
        let test_int: Option<i32> = request.extensions.get();
        assert_eq!(test_int, Some(100));
        
        // Add values to the extended layer (these should NOT affect the original)
        request.extensions.insert(999i32); // Try to override parent value
        request.extensions.insert(2.71f64); // Add new type
        
        // Send back a JSON response
        let response_json = json!({"data": {"hello": "world"}});
        response.send_response(JsonResponse {
            extensions: request.extensions,
            responses: Box::pin(stream::once(async move { response_json })),
        });
    });

    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(BytesToJsonLayer)
        .service(mock_service);

    // Create our Extensions and store some test data
    let mut extensions = crate::Extensions::default();
    extensions.insert("test_context".to_string());
    extensions.insert(100i32); // Add an integer for testing

    // Create a test bytes request with JSON content and Extensions
    let json_bytes = serde_json::to_vec(&json!({"query": "{ hello }"}))
        .expect("JSON serialization should succeed");
    
    let bytes_req = BytesRequest {
        extensions,
        body: Bytes::from(json_bytes),
    };

    // Call the service and verify the response
    let response = service
        .oneshot(bytes_req)
        .await
        .expect("response expected");

    // Verify the original Extensions were preserved in the response
    let original_string: Option<String> = response.extensions.get();
    assert_eq!(original_string, Some("test_context".to_string()));
    
    let original_int: Option<i32> = response.extensions.get();
    assert_eq!(original_int, Some(100)); // Original value, not the 999 from inner service
    
    // Inner service values should NOT be visible (they were in an extended layer)
    let inner_float: Option<f64> = response.extensions.get();
    assert_eq!(inner_float, None); // Inner service's f64 should not be visible

    // Collect the response stream to verify the JSON response
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item");

    let response_json: JsonValue = serde_json::from_slice(&response_bytes)
        .expect("Response should be valid JSON");
    
    let expected_response = json!({"data": {"hello": "world"}});
    assert_eq!(response_json, expected_response);
} 