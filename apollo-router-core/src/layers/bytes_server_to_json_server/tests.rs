use bytes::Bytes;
use futures::StreamExt;
use futures::stream;
use serde_json::json;

use crate::assert_error;
use crate::json::JsonValue;
use crate::layers::bytes_server_to_json_server::BytesToJsonLayer;
use crate::layers::bytes_server_to_json_server::Error as BytesToJsonError;
use crate::services::bytes_server::Request as BytesRequest;
use crate::services::json_server::Response as JsonResponse;
use crate::test_utils::TowerTest;

#[tokio::test]
async fn test_bytes_to_json_conversion() {
    let layer = BytesToJsonLayer;

    // Create a test bytes request with JSON content
    let json_bytes = serde_json::to_vec(&json!({"query": "{ hello }", "variables": {}}))
        .expect("JSON serialization should succeed");

    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(json_bytes),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_req, |mut downstream| async move {
            downstream.allow(1);
            let (request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            // Verify the JSON was parsed correctly
            let expected_json = json!({"query": "{ hello }", "variables": {}});
            assert_eq!(request.body, expected_json);

            // Send back a JSON response
            let response_json = json!({"data": {"hello": "world"}});
            response.send_response(JsonResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async move { Ok(response_json) })),
            });
        })
        .await
        .expect("Test should succeed");

    // Collect the response stream
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item")
        .expect("response should be Ok");

    // Parse the response back to JSON to verify it's correct
    let response_json: JsonValue =
        serde_json::from_slice(&response_bytes).expect("Response should be valid JSON");

    let expected_response = json!({"data": {"hello": "world"}});
    assert_eq!(response_json, expected_response);
}

#[tokio::test]
async fn test_invalid_json_bytes() {
    let layer = BytesToJsonLayer;

    // Create a test bytes request with invalid JSON content
    let invalid_json_bytes = b"invalid json content";

    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(&invalid_json_bytes[..]),
    };

    let result = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_req, |mut _downstream| async move {
            // This test should fail during JSON parsing, so downstream won't be called
        })
        .await;

    // Verify it's specifically a JSON deserialization error using the assert_error! macro
    assert_error!(
        result,
        BytesToJsonError,
        BytesToJsonError::JsonDeserialization { .. }
    );
}

#[tokio::test]
async fn test_empty_json_object() {
    let layer = BytesToJsonLayer;

    // Create a test bytes request with empty JSON object
    let json_bytes = b"{}";

    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(&json_bytes[..]),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_req, |mut downstream| async move {
            downstream.allow(1);
            let (request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            // Verify the empty JSON object was parsed correctly
            let expected_json = json!({});
            assert_eq!(request.body, expected_json);

            // Send back a JSON response
            let response_json = json!({"status": "ok"});
            response.send_response(JsonResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async move { Ok(response_json) })),
            });
        })
        .await
        .expect("Test should succeed");

    // Collect the response stream
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item")
        .expect("response should be Ok");

    // Parse the response back to JSON to verify it's correct
    let response_json: JsonValue =
        serde_json::from_slice(&response_bytes).expect("Response should be valid JSON");

    let expected_response = json!({"status": "ok"});
    assert_eq!(response_json, expected_response);
}

#[tokio::test]
async fn test_extensions_passthrough() {
    let layer = BytesToJsonLayer;

    // Create our Extensions and store some test data
    let mut extensions = crate::Extensions::default();
    extensions.insert("test_context".to_string());
    extensions.insert(100i32);

    // Create a test bytes request with JSON content and Extensions
    let json_bytes = serde_json::to_vec(&json!({"query": "{ hello }"}))
        .expect("JSON serialization should succeed");

    let bytes_req = BytesRequest {
        extensions: extensions.clone(),
        body: Bytes::from(json_bytes),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_req, |mut downstream| async move {
            downstream.allow(1);
            let (mut request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

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
                responses: Box::pin(stream::once(async move { Ok(response_json) })),
            });
        })
        .await
        .expect("Test should succeed");

    // Verify the original Extensions were preserved in the response (parent values take precedence)
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
        .expect("response stream should have at least one item")
        .expect("response should be Ok");

    let response_json: JsonValue =
        serde_json::from_slice(&response_bytes).expect("Response should be valid JSON");

    let expected_response = json!({"data": {"hello": "world"}});
    assert_eq!(response_json, expected_response);
}

#[tokio::test]
async fn test_downstream_service_error() {
    let layer = BytesToJsonLayer;

    // Create a test bytes request with valid JSON content
    let json_bytes =
        serde_json::to_vec(&json!({"test": "data"})).expect("JSON serialization should succeed");

    let bytes_req = BytesRequest {
        extensions: crate::Extensions::default(),
        body: Bytes::from(json_bytes),
    };

    let result = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_req, |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            response.send_error(tower::BoxError::from("Downstream JSON service failed"));
        })
        .await;

    assert!(
        result.is_err(),
        "Should return error when downstream service fails"
    );

    // Since we use BoxError directly, check the error message
    if let Err(error) = result {
        let error_message = error.to_string();
        assert!(error_message.contains("Downstream JSON service failed"));
    }
}
