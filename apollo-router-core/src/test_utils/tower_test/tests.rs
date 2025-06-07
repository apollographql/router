use super::*;
use bytes::Bytes;
use futures::stream;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;

use crate::layers::http_to_bytes::HttpToBytesLayer;
use crate::services::bytes_server::Response as BytesResponse;
use crate::test_utils::LayerTestBuilder;

#[tokio::test]
async fn test_clean_builder_api_oneshot() {
    let layer = HttpToBytesLayer;

    let http_req = http::Request::builder()
        .uri("http://example.com")
        .body(UnsyncBoxBody::new(
            http_body_util::Full::from("test body").map_err(Into::into),
        ))
        .unwrap();

    let result: http::Response<_> = LayerTestBuilder::new()
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
                responses: Box::pin(stream::once(async { Bytes::from("test response") })),
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

    let result = LayerTestBuilder::new()
        .layer(layer)
        .timeout(Duration::from_millis(100))
        .oneshot(http_req, |mut _downstream| async move {
            // Don't provide any response - this will cause a timeout
            tokio::time::sleep(Duration::from_millis(200)).await;
        })
        .await;

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    // Test can timeout or service can close due to mock not responding
    assert!(error_msg.contains("timed out") || error_msg.contains("service closed"));
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

    let result = LayerTestBuilder::new()
        .layer(layer)
        .timeout(Duration::from_millis(100))
        .oneshot(http_req, |mut _downstream| async move {
            panic!("This should be caught");
        })
        .await;

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("panicked"));
}

#[tokio::test]
async fn test_clean_builder_api_custom_test() {
    let layer = HttpToBytesLayer;

    let result = LayerTestBuilder::new()
        .layer(layer)
        .test(
            |mut service| async move {
                let http_req = http::Request::builder()
                    .uri("http://example.com")
                    .body(UnsyncBoxBody::new(
                        http_body_util::Full::from("test body").map_err(Into::into),
                    ))
                    .unwrap();
                service.oneshot(http_req).await
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
                    responses: Box::pin(stream::once(async { Bytes::from("test response") })),
                });
            },
        )
        .await
        .expect("Test should succeed");

    let collected = result.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(collected, "test response".as_bytes());
}
