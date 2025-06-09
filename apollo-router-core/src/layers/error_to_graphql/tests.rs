use super::*;
use crate::test_utils::TowerTest;
use futures::{stream, StreamExt};
use serde_json::json;
use tower::BoxError;

#[tokio::test]
async fn test_layer_converts_service_errors_to_graphql() {
    let layer = ErrorToGraphQLLayer;
    
    let result = TowerTest::builder()
        .layer(layer)
        .oneshot((), |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("should receive request");
            
            // Send an error from the downstream service
            response.send_error(BoxError::from(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Service failed"
            )));
        })
        .await;
    
    // The layer should convert the error to a successful GraphQL response
    let response = result.expect("Layer should convert error to GraphQL response");
    
    // Collect the response stream
    let responses: Vec<_> = response.responses.collect().await;
    assert_eq!(responses.len(), 1);
    
    let graphql_response = &responses[0];
    
    // Verify it's a proper GraphQL error response
    assert!(graphql_response["data"].is_null());
    assert!(graphql_response["errors"].is_array());
    assert_eq!(graphql_response["errors"].as_array().unwrap().len(), 1);
    
    let error = &graphql_response["errors"][0];
    assert!(error["message"].as_str().unwrap().contains("Service failed"));
    assert!(error["extensions"]["code"].is_string());
}

#[tokio::test] 
async fn test_layer_passes_through_successful_responses() {
    let layer = ErrorToGraphQLLayer;
    
    let response = TowerTest::builder()
        .layer(layer)
        .oneshot((), |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("should receive request");
            
            // Send a successful response from the downstream service
            response.send_response(JsonResponse {
                extensions: crate::Extensions::default(),
                responses: Box::pin(stream::once(async { json!({"data": "success"}) })),
            });
        })
        .await
        .expect("Layer should pass through successful response");
    
    // Collect the response stream
    let responses: Vec<_> = response.responses.collect().await;
    assert_eq!(responses.len(), 1);
    
    let json_response = &responses[0];
    assert_eq!(json_response["data"], "success");
}