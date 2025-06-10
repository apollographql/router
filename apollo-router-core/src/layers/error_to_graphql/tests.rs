use super::*;
use crate::test_utils::TowerTest;
use futures::{stream, StreamExt};
use serde_json::json;
use tower::BoxError;

// Test error type using apollo-router-error derive macros
#[derive(Debug, thiserror::Error, miette::Diagnostic, apollo_router_error::Error)]
pub enum TestGraphQLError {
    #[error("Test service connection failed to {endpoint}")]
    #[diagnostic(
        code(apollo_router::test::service_connection_failed),
        help("Verify the test endpoint is reachable")
    )]
    ServiceConnectionFailed {
        #[extension("failedEndpoint")]
        endpoint: String,
        #[extension("retryCount")]
        retry_count: u32,
    },

    #[error("Test configuration error: {message}")]
    #[diagnostic(
        code(apollo_router::test::config_error),
        help("Check your test configuration")
    )]
    ConfigError {
        #[extension("configMessage")]
        message: String,
    },
}

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
    
    let graphql_response = &responses[0].as_ref().expect("response should be Ok");
    
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
                responses: Box::pin(stream::once(async { Ok(json!({"data": "success"})) })),
            });
        })
        .await
        .expect("Layer should pass through successful response");
    
    // Collect the response stream
    let responses: Vec<_> = response.responses.collect().await;
    assert_eq!(responses.len(), 1);
    
    let json_response = &responses[0].as_ref().expect("response should be Ok");
    assert_eq!(json_response["data"], "success");
}

#[tokio::test]
async fn test_layer_converts_apollo_router_errors_properly() {
    let layer = ErrorToGraphQLLayer;
    
    let result = TowerTest::builder()
        .layer(layer)
        .oneshot((), |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("should receive request");
            
            // Send an Apollo Router error with extensions
            let apollo_error = TestGraphQLError::ServiceConnectionFailed {
                endpoint: "https://api.example.com".to_string(),
                retry_count: 3,
            };
            
            response.send_error(BoxError::from(apollo_error));
        })
        .await;
    
    // The layer should convert the error to a GraphQL response with proper error code
    let response = result.expect("Layer should convert Apollo Router error to GraphQL response");
    
    // Collect the response stream
    let responses: Vec<_> = response.responses.collect().await;
    assert_eq!(responses.len(), 1);
    
    let graphql_response = &responses[0].as_ref().expect("response should be Ok");
    
    // Verify it's a proper GraphQL error response
    assert!(graphql_response["data"].is_null());
    assert!(graphql_response["errors"].is_array());
    assert_eq!(graphql_response["errors"].as_array().unwrap().len(), 1);
    
    let error = &graphql_response["errors"][0];
    
    // Verify the message contains our test error details
    assert!(error["message"].as_str().unwrap().contains("Test service connection failed"));
    assert!(error["message"].as_str().unwrap().contains("https://api.example.com"));
    
    // Verify the error code was properly converted from Apollo Router format to GraphQL format
    assert_eq!(
        error["extensions"]["code"].as_str().unwrap(),
        "APOLLO_ROUTER_TEST_SERVICE_CONNECTION_FAILED"
    );
    
    // Verify service name is set correctly
    assert_eq!(
        error["extensions"]["service"].as_str().unwrap(),
        "apollo-router"
    );
    
    // Verify extension fields are properly included
    let details = &error["extensions"]["details"];
    assert_eq!(
        details["failedEndpoint"].as_str().unwrap(),
        "https://api.example.com"
    );
    assert_eq!(details["retryCount"].as_u64().unwrap(), 3);
    
    // Verify timestamp is present
    assert!(error["extensions"]["timestamp"].is_string());
}

#[tokio::test]
async fn test_layer_converts_nested_apollo_router_errors() {
    let layer = ErrorToGraphQLLayer;
    
    let result = TowerTest::builder()
        .layer(layer)
        .oneshot((), |mut downstream| async move {
            downstream.allow(1);
            let (_request, response) = downstream
                .next_request()
                .await
                .expect("should receive request");
            
            // Create a nested error structure: Box<Arc<TestGraphQLError>>
            let apollo_error = TestGraphQLError::ConfigError {
                message: "Invalid nested configuration".to_string(),
            };
            let arc_error = std::sync::Arc::new(apollo_error);
            let boxed_arc_error = BoxError::from(arc_error);
            
            response.send_error(boxed_arc_error);
        })
        .await;
    
    // The layer should handle nested error structures properly
    let response = result.expect("Layer should convert nested Apollo Router error to GraphQL response");
    
    // Collect the response stream
    let responses: Vec<_> = response.responses.collect().await;
    assert_eq!(responses.len(), 1);
    
    let graphql_response = &responses[0].as_ref().expect("response should be Ok");
    
    // Verify it's a proper GraphQL error response
    assert!(graphql_response["data"].is_null());
    assert!(graphql_response["errors"].is_array());
    assert_eq!(graphql_response["errors"].as_array().unwrap().len(), 1);
    
    let error = &graphql_response["errors"][0];
    
    // Verify the nested error was properly converted
    assert!(error["message"].as_str().unwrap().contains("Test configuration error"));
    assert!(error["message"].as_str().unwrap().contains("Invalid nested configuration"));
    
    // Verify the error code was properly converted
    assert_eq!(
        error["extensions"]["code"].as_str().unwrap(),
        "APOLLO_ROUTER_TEST_CONFIG_ERROR"
    );
    
    // Verify extension fields are properly included
    let details = &error["extensions"]["details"];
    assert_eq!(
        details["configMessage"].as_str().unwrap(),
        "Invalid nested configuration"
    );
}