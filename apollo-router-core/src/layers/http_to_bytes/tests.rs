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
        
        // Verify the extensions were passed through from HTTP request
        let test_value: Option<String> = request.extensions.get();
        assert_eq!(test_value, Some("test_context".to_string()));
        
        // Store a response value in extensions
        let mut response_extensions = request.extensions;
        response_extensions.insert("response_data".to_string());
        
        response.send_response(BytesResponse {
            extensions: response_extensions,
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

    // Verify our Extensions were stored back in the HTTP response
    let response_extensions = http_response
        .extensions()
        .get::<crate::Extensions>()
        .expect("Extensions should be present in response");
    
    let response_data: Option<String> = response_extensions.get();
    assert_eq!(response_data, Some("response_data".to_string()));

    let collected = http_response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}
