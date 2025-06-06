use crate::layers::http_to_bytes::HttpToBytesLayer;
use crate::services::bytes_server::{Request as BytesRequest, Response as BytesResponse};
use bytes::Bytes;
use futures::stream;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use tower::ServiceBuilder;
use tower::{Service, ServiceExt};
use tower_test::mock;

#[tokio::test]
async fn test_http_to_bytes_conversion() {
    let (mut mock_service, mut handle) = mock::pair::<BytesRequest, BytesResponse>();
    // Set up the mock to expect a bytes request and return a bytes response
    handle.allow(1);
    let _ = tokio::task::spawn(async move {
        let (request, response) = handle.next_request().await.expect("service must not fail");
        assert_eq!(request.body, "test body".as_bytes());
        response.send_response(BytesResponse {
            extensions: crate::Extensions::default(),
            responses: Box::pin(stream::once(async { Bytes::from("test response") })),
        });
    });

    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(HttpToBytesLayer)
        .service(mock_service);

    // Create a test HTTP request
    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    // Call the service and verify the response
    let body = service
        .oneshot(http_req)
        .await
        .expect("response expected")
        .into_body();

    let collected = body.collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}

#[tokio::test]
async fn test_extensions_passthrough() {
    let (mut mock_service, mut handle) = mock::pair::<BytesRequest, BytesResponse>();
    
    // Set up the mock to verify extensions are passed through
    handle.allow(1);
    let _ = tokio::task::spawn(async move {
        let (request, response) = handle.next_request().await.expect("service must not fail");
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
            responses: Box::pin(stream::once(async { Bytes::from("test response") })),
        });
    });

    // Set up the service under test
    let mut service = ServiceBuilder::new()
        .layer(HttpToBytesLayer)
        .service(mock_service);

    // Create our Extensions and store some test data
    let mut extensions = crate::Extensions::default();
    extensions.insert("test_context".to_string());
    extensions.insert(42i32); // Add an integer for testing

    // Create a test HTTP request with our Extensions stored in HTTP extensions
    let mut http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();
    
    // Store our Extensions in the HTTP request extensions
    http_req.extensions_mut().insert(extensions);

    // Call the service and verify the response
    let http_response = service
        .oneshot(http_req)
        .await
        .expect("response expected");

    // Verify the original Extensions were returned in the HTTP response
    let response_extensions = http_response
        .extensions()
        .get::<crate::Extensions>()
        .expect("Extensions should be present in response");
    
    // Original values should be preserved exactly
    let original_string: Option<String> = response_extensions.get();
    assert_eq!(original_string, Some("test_context".to_string()));
    
    let original_int: Option<i32> = response_extensions.get();
    assert_eq!(original_int, Some(42)); // Original value, not the 999 from inner service
    
    // Inner service values should NOT be visible (they were in an extended layer)
    let inner_float: Option<f64> = response_extensions.get();
    assert_eq!(inner_float, None); // Inner service's f64 should not be visible

    let collected = http_response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}
