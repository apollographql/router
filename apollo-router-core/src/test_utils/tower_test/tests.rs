use bytes::Bytes;
use futures::stream;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;

use super::*;
use crate::layers::http_server_to_bytes_server::HttpToBytesLayer;
use crate::services::bytes_server::Response as BytesResponse;

#[tokio::test]
async fn test_clean_builder_api_oneshot() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let result: http::Response<_> = TowerTest::builder()
        .layer(layer)
        .oneshot(http_req, |mut downstream| async move {
            downstream.allow(1);
            let (request, response) = downstream
                .next_request()
                .await
                .expect("service must not fail");
            assert_eq!(request.body, "test body".as_bytes());
            response.send_response(BytesResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async { Ok(Bytes::from("test response")) })),
            });
        })
        .await
        .expect("Test should succeed");

    let collected = result.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}

#[tokio::test]
async fn test_clean_builder_api_with_timeout() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let result = TowerTest::builder()
        .layer(layer)
        .timeout(Duration::from_millis(100))
        .oneshot(http_req, |mut _downstream| async move {
            // Don't provide any response - this will cause a timeout
            tokio::time::sleep(Duration::from_millis(200)).await;
        })
        .await;

    assert!(result.is_err());
    if let Err(error) = result {
        let error_msg = error.to_string();
        // Test can timeout or service can close due to mock not responding
        assert!(error_msg.contains("timed out") || error_msg.contains("service closed"));
    }
}

#[tokio::test]
async fn test_clean_builder_api_panic_detection() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let result = TowerTest::builder()
        .layer(layer)
        .timeout(Duration::from_millis(100))
        .oneshot(http_req, |mut _downstream| async move {
            panic!("This should be caught");
        })
        .await;

    assert!(result.is_err());
    if let Err(error) = result {
        let error_msg = error.to_string();
        assert!(error_msg.contains("panicked"));
    }
}

#[tokio::test]
async fn test_clean_builder_api_custom_test() {
    let layer = HttpToBytesLayer;

    let result = TowerTest::builder()
        .layer(layer)
        .test(
            |mut service| async move {
                let http_req = http::Request::builder()
                    .uri("http://example.com")
                    .body(UnsyncBoxBody::new(
                        http_body_util::Full::from("test body").map_err(Into::into),
                    ))
                    .unwrap();
                service.ready().await?;
                service.call(http_req).await
            },
            |mut downstream| async move {
                downstream.allow(1);
                let (request, response) = downstream
                    .next_request()
                    .await
                    .expect("service must not fail");
                assert_eq!(request.body, "test body".as_bytes());
                response.send_response(BytesResponse {
                    extensions: crate::Extensions::default(),
                    responses: Box::pin(stream::once(async { Ok(Bytes::from("test response")) })),
                });
            },
        )
        .await
        .expect("Test should succeed");

    let collected = result.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}

#[tokio::test]
async fn test_service_mock_functionality() {
    use std::time::Duration;

    use tower::ServiceExt;

    // Create a mock service that expects one request
    let mut mock_service = TowerTest::builder()
        .timeout(Duration::from_secs(1))
        .service(|mut handle| async move {
            handle.allow(1);
            let (request, response) = handle.next_request().await.expect("service must not fail");
            assert_eq!(request, "test request");
            response.send_response("mock response");
        });

    // Use the mock service properly with ready() + call()
    mock_service.ready().await.expect("service should be ready");
    let response = mock_service
        .call("test request")
        .await
        .expect("service call should succeed");
    assert_eq!(response, "mock response");

    // Give expectations time to complete
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn test_service_mock_with_constructor() {
    use std::time::Duration;

    use tower::Service;
    use tower::ServiceExt;

    // Create a mock service
    let mock_service = TowerTest::builder()
        .timeout(Duration::from_secs(1))
        .service(|mut handle| async move {
            handle.allow(2);

            // First request
            let (request, response) = handle.next_request().await.expect("service must not fail");
            assert_eq!(request, "request1");
            response.send_response("response1");

            // Second request
            let (request, response) = handle.next_request().await.expect("service must not fail");
            assert_eq!(request, "request2");
            response.send_response("response2");
        });

    // Example of using the mock service in a service constructor
    let mut wrapper_service = ExampleService::new(mock_service);

    // Make requests through the wrapper service using ready() + call()
    wrapper_service
        .ready()
        .await
        .expect("service should be ready");
    let response1 = wrapper_service
        .call("request1")
        .await
        .expect("first call should succeed");
    assert_eq!(response1, "response1");

    wrapper_service
        .ready()
        .await
        .expect("service should be ready");
    let response2 = wrapper_service
        .call("request2")
        .await
        .expect("second call should succeed");
    assert_eq!(response2, "response2");
}

#[tokio::test]
#[should_panic(expected = "Mock service expectations timed out")]
async fn test_service_mock_panic_on_timeout() {
    use std::time::Duration;

    // Create a mock service with a very short timeout and slow expectations
    let _mock_service = TowerTest::builder()
        .timeout(Duration::from_millis(10))
        .service(
            |mut _handle: ::tower_test::mock::Handle<String, String>| async move {
                // Sleep longer than the timeout
                tokio::time::sleep(Duration::from_millis(100)).await;
            },
        );

    // Wait a bit for the timeout to occur, then drop the service
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Mock service will panic on drop due to timeout
}

#[tokio::test]
async fn test_service_mock_normal_lifecycle() {
    use std::time::Duration;

    // Test that mock services work normally and can be dropped safely
    // when expectations complete successfully
    let mock_service = TowerTest::builder().service(
        |mut handle: ::tower_test::mock::Handle<String, String>| async move {
            handle.allow(1);
            let (request, response) = handle.next_request().await.expect("service must not fail");
            assert_eq!(request, "test".to_string());
            response.send_response("ok".to_string());
        },
    );

    // Clone the service to test that cloning works
    let mut cloned_service = mock_service.clone();

    // Use the service normally
    cloned_service
        .ready()
        .await
        .expect("service should be ready");
    let response = cloned_service
        .call("test".to_string())
        .await
        .expect("service call should succeed");
    assert_eq!(response, "ok".to_string());

    // Give time for expectations to complete
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both services should be droppable without panic since expectations completed
    drop(mock_service);
    drop(cloned_service);

    // Test passes if we reach here without panic
}

#[tokio::test]
async fn test_service_mock_clone() {
    use std::time::Duration;

    use tower::ServiceExt;

    // Create a mock service that expects multiple requests
    let mock_service = TowerTest::builder()
        .timeout(Duration::from_secs(1))
        .service(|mut handle| async move {
            handle.allow(3);

            // Handle requests from different clones
            for i in 1..=3 {
                let (request, response) =
                    handle.next_request().await.expect("service must not fail");
                assert_eq!(request, format!("request{}", i));
                response.send_response(format!("response{}", i));
            }
        });

    // Clone the mock service
    let mut clone1 = mock_service.clone();
    let mut clone2 = mock_service.clone();
    let mut original = mock_service;

    // Use all three services (original + 2 clones)
    original.ready().await.expect("service should be ready");
    let response1 = original
        .call("request1")
        .await
        .expect("first call should succeed");
    assert_eq!(response1, "response1");

    clone1.ready().await.expect("service should be ready");
    let response2 = clone1
        .call("request2")
        .await
        .expect("second call should succeed");
    assert_eq!(response2, "response2");

    clone2.ready().await.expect("service should be ready");
    let response3 = clone2
        .call("request3")
        .await
        .expect("third call should succeed");
    assert_eq!(response3, "response3");

    // Give expectations time to complete
    tokio::time::sleep(Duration::from_millis(10)).await;
}
