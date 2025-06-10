use crate::layers::http_server_to_bytes_server::HttpToBytesLayer;
use crate::services::bytes_server::Response as BytesResponse;
use crate::test_utils::TowerTest;
use bytes::Bytes;
use futures::stream;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;

#[tokio::test]
async fn test_http_to_bytes_conversion() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(http_req, |mut downstream| async move {
            downstream.allow(1);
            let (request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            // Verify the request was transformed correctly
            assert_eq!(request.body, "test body".as_bytes());

            // Send back a bytes response
            response.send_response(BytesResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async { Ok(Bytes::from("test response")) })),
            });
        })
        .await
        .expect("Test should succeed");

    let collected = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}

#[tokio::test]
async fn test_extensions_passthrough() {
    let layer = HttpToBytesLayer;

    // Create our Extensions and store some test data
    let mut extensions = crate::Extensions::default();
    extensions.insert("test_context".to_string());
    extensions.insert(42i32);

    // Create a test HTTP request with our Extensions converted to HTTP extensions
    let mut http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    // Convert our Extensions to http::Extensions and set it
    let http_extensions: http::Extensions = extensions.clone().into();
    *http_req.extensions_mut() = http_extensions;

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(http_req, |mut downstream| async move {
            downstream.allow(1);
            let (mut request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            assert_eq!(request.body, "test body".as_bytes());

            // Verify the extensions were extended from HTTP request (parent values accessible)
            let test_value: Option<String> = request.extensions.get();
            assert_eq!(test_value, Some("test_context".to_string()));

            let test_int: Option<i32> = request.extensions.get();
            assert_eq!(test_int, Some(42));

            // Add values to the extended layer (these should NOT affect the original)
            request.extensions.insert(999i32); // Try to override parent value
            request.extensions.insert(3.14f64); // Add new type

            response.send_response(BytesResponse {
                extensions: request.extensions,
                responses: Box::pin(stream::once(async { Ok(Bytes::from("test response")) })),
            });
        })
        .await
        .expect("Test should succeed");

    // Verify response preserves original extensions (parent values take precedence)
    let original_extensions: crate::Extensions = response.extensions().clone().into();

    let preserved_string: Option<String> = original_extensions.get();
    assert_eq!(preserved_string, Some("test_context".to_string()));

    let preserved_int: Option<i32> = original_extensions.get();
    assert_eq!(preserved_int, Some(42)); // Original value, not 999

    let preserved_float: Option<f64> = original_extensions.get();
    assert_eq!(preserved_float, None); // Inner additions not visible
}

#[tokio::test]
async fn test_downstream_service_error() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let result = TowerTest::builder()
        .layer(layer)
        .oneshot(http_req, |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            response.send_error(tower::BoxError::from("Downstream service failed"));
        })
        .await;

    assert!(
        result.is_err(),
        "Should return error when downstream service fails"
    );

    // Since we use BoxError directly, we can check the error message
    if let Err(error) = result {
        let error_message = error.to_string();
        assert!(error_message.contains("Downstream service failed"));
    }
}

#[tokio::test]
async fn test_empty_body() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Empty::<Bytes>::new().map_err(Into::into),
        ))
        .unwrap();

    let response = TowerTest::builder()
        .layer(layer)
        .oneshot(http_req, |mut downstream| async move {
            downstream.allow(1);
            let (request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");

            // Verify empty body becomes empty bytes
            assert_eq!(request.body, Bytes::new());

            // Send back a response
            response.send_response(BytesResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async { Ok(Bytes::from("empty response")) })),
            });
        })
        .await
        .expect("Test should succeed");

    let collected = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "empty response".as_bytes());
}

// Note: Error testing for HttpToBytesError::HttpResponseBuilder would require
// more complex setup to trigger http::Response::builder() failures.
// In practice, this error is rare since we use simple, valid response parameters.
//
// If we needed to test this error, we would use:
// assert_error!(result, HttpToBytesError, HttpToBytesError::HttpResponseBuilder { .. });
