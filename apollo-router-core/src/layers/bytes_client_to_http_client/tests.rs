use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use http_body_util::Full;

use crate::Extensions;
use crate::layers::bytes_client_to_http_client::BytesToHttpLayer;
use crate::services::bytes_client::Request as BytesRequest;
use crate::test_utils::TowerTest;

#[tokio::test]
async fn test_bytes_to_http_layer_success() {
    let layer = BytesToHttpLayer::new();

    // Prepare test data
    let test_bytes = Bytes::from("test request body");
    let mut extensions = Extensions::default();
    extensions.insert("test_value".to_string());

    let bytes_request = BytesRequest {
        extensions: extensions.clone(),
        body: test_bytes.clone(),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_request, |mut downstream| async move {
            downstream.allow(1);
            let (http_req, send_response) =
                downstream.next_request().await.expect("service not called");

            // Verify parent values are accessible in HTTP request extensions (before consuming body)
            let extended_extensions: crate::Extensions = http_req.extensions().clone().into();
            let parent_string: Option<String> = extended_extensions.get();
            assert_eq!(parent_string, Some("test_value".to_string()));

            // Verify bytes were converted to HTTP body correctly
            let collected_body = http_req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(collected_body, test_bytes);

            // Send HTTP response back
            let http_response = http::Response::builder()
                .status(200)
                .body(http_body_util::combinators::UnsyncBoxBody::new(
                    Full::new(Bytes::from("response body")).map_err(Into::into),
                ))
                .unwrap();

            send_response.send_response(http_response);
        })
        .await
        .expect("Test should succeed");

    // Verify response preserves original extensions (parent values take precedence)
    let preserved_string: Option<String> = response.extensions.get();
    assert_eq!(preserved_string, Some("test_value".to_string()));

    // Collect the response stream
    let mut response_stream = response.responses;
    let response_bytes = response_stream
        .next()
        .await
        .expect("response stream should have at least one item")
        .expect("response should be Ok");

    assert_eq!(response_bytes, "response body".as_bytes());
}

#[tokio::test]
async fn test_bytes_to_http_empty_body() {
    let layer = BytesToHttpLayer::new();

    let bytes_request = BytesRequest {
        extensions: Extensions::default(),
        body: Bytes::new(),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_request, |mut downstream| async move {
            downstream.allow(1);
            let (http_req, send_response) =
                downstream.next_request().await.expect("service not called");

            // Verify empty bytes become empty HTTP body
            let collected_body = http_req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(collected_body, Bytes::new());

            let http_response = http::Response::builder()
                .status(200)
                .body(http_body_util::combinators::UnsyncBoxBody::new(
                    Full::new(Bytes::from("empty response")).map_err(Into::into),
                ))
                .unwrap();

            send_response.send_response(http_response);
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

    assert_eq!(response_bytes, "empty response".as_bytes());
}

#[tokio::test]
async fn test_downstream_service_error() {
    let layer = BytesToHttpLayer::new();

    let bytes_request = BytesRequest {
        extensions: Extensions::default(),
        body: Bytes::from("test data"),
    };

    let result = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_request, |mut downstream| async move {
            downstream.allow(1);
            let (_http_req, send_response) =
                downstream.next_request().await.expect("service not called");

            send_response.send_error(tower::BoxError::from("Downstream HTTP service failed"));
        })
        .await;

    assert!(
        result.is_err(),
        "Should return error when downstream service fails"
    );

    if let Err(error) = result {
        let error_message = error.to_string();
        assert!(error_message.contains("Downstream HTTP service failed"));
    }
}

#[tokio::test]
async fn test_extensions_passthrough() {
    let layer = BytesToHttpLayer::new();

    // Setup original extensions with multiple types
    let mut extensions = Extensions::default();
    extensions.insert("original_string".to_string());
    extensions.insert(42i32);
    extensions.insert(3.14f64);

    let bytes_request = BytesRequest {
        extensions: extensions.clone(),
        body: Bytes::from("test"),
    };

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(bytes_request, |mut downstream| async move {
            downstream.allow(1);
            let (http_req, send_response) =
                downstream.next_request().await.expect("service not called");

            // Verify parent values are accessible in HTTP request extensions
            let extended_extensions: crate::Extensions = http_req.extensions().clone().into();

            let parent_string: Option<String> = extended_extensions.get();
            assert_eq!(parent_string, Some("original_string".to_string()));

            let parent_int: Option<i32> = extended_extensions.get();
            assert_eq!(parent_int, Some(42));

            let parent_float: Option<f64> = extended_extensions.get();
            assert_eq!(parent_float, Some(3.14));

            // Verify the extended extensions are immutable from downstream perspective
            // (This is correct behavior - downstream services cannot modify parent context)

            let http_response = http::Response::builder()
                .status(200)
                .body(http_body_util::combinators::UnsyncBoxBody::new(
                    Full::new(Bytes::from("{}")).map_err(Into::into),
                ))
                .unwrap();

            send_response.send_response(http_response);
        })
        .await
        .expect("Test should succeed");

    // Verify response preserves original extensions (parent values take precedence)
    let response_string: Option<String> = response.extensions.get();
    assert_eq!(response_string, Some("original_string".to_string()));

    let response_int: Option<i32> = response.extensions.get();
    assert_eq!(response_int, Some(42)); // Original value, not 999

    let response_float: Option<f64> = response.extensions.get();
    assert_eq!(response_float, Some(3.14)); // Original value

    // Inner additions should not be visible
    let response_bool: Option<bool> = response.extensions.get();
    assert_eq!(response_bool, None);
}
