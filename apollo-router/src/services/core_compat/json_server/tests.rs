use futures::{StreamExt, stream};
use serde_json::json;
use std::sync::Arc;

use super::{
    RequestMetadata, ResponseMetadata, core_json_request_to_supergraph_request,
    core_json_response_to_supergraph_response, supergraph_request_to_core_json_request,
    supergraph_response_to_core_json_response,
};
use crate::Context;
use crate::graphql;
use crate::services::supergraph::{Request as SupergraphRequest, Response as SupergraphResponse};

use apollo_router_core::services::json_server::{
    Request as CoreJsonRequest, Response as CoreJsonResponse,
};
use http::StatusCode;

fn create_sample_graphql_request() -> graphql::Request {
    graphql::Request::builder()
        .query("query { hello }")
        .operation_name("TestQuery")
        .variable("test_var", "test_value")
        .extension("test_ext", json!({"key": "value"}))
        .build()
}

fn create_sample_graphql_response() -> graphql::Response {
    graphql::Response::builder()
        .data(json!({"hello": "world"}))
        .extension("response_ext", json!({"status": "ok"}))
        .build()
}

#[tokio::test]
async fn test_core_json_request_to_supergraph_request() {
    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();

    let metadata = RequestMetadata {
        http_parts: http::Request::builder()
            .method("POST")
            .uri("http://example.com/graphql")
            .header("content-type", "application/json")
            .body(())
            .unwrap()
            .into_parts()
            .0,
        context: context.clone(),
    };

    // Create a core JSON request with GraphQL query as JSON
    let graphql_request = create_sample_graphql_request();
    let json_body = serde_json::to_value(graphql_request.clone()).unwrap();

    let mut core_request = CoreJsonRequest {
        extensions: Default::default(),
        body: json_body,
    };

    core_request.extensions.insert(Arc::new(metadata));

    let supergraph_request = core_json_request_to_supergraph_request(core_request).unwrap();

    assert_eq!(supergraph_request.supergraph_request.method(), "POST");
    assert_eq!(
        supergraph_request.supergraph_request.uri(),
        "http://example.com/graphql"
    );
    assert_eq!(
        supergraph_request
            .supergraph_request
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        supergraph_request.supergraph_request.body(),
        &graphql_request
    );

    // Compare specific values in context instead of direct Context comparison
    let stored_value = supergraph_request
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "test_value");
}

#[tokio::test]
async fn test_supergraph_request_to_core_json_request() {
    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();

    let graphql_request = create_sample_graphql_request();
    let expected_json = serde_json::to_value(&graphql_request).unwrap();

    let http_request = http::Request::builder()
        .method("POST")
        .uri("http://example.com/graphql")
        .header("content-type", "application/json")
        .body(graphql_request)
        .unwrap();

    let supergraph_request = SupergraphRequest {
        supergraph_request: http_request,
        context: context.clone(),
    };

    let core_request = supergraph_request_to_core_json_request(supergraph_request).unwrap();

    assert_eq!(core_request.body, expected_json);

    // Verify metadata is stored in extensions
    let metadata_arc = core_request
        .extensions
        .get::<Arc<RequestMetadata>>()
        .unwrap();
    assert_eq!(metadata_arc.http_parts.method, "POST");
    assert_eq!(metadata_arc.http_parts.uri, "http://example.com/graphql");
    let stored_value = metadata_arc
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "test_value");
}

#[tokio::test]
async fn test_core_json_response_to_supergraph_response() {
    let context = Context::new();
    context
        .insert("response_key", "response_value".to_string())
        .unwrap();

    let metadata = ResponseMetadata {
        http_parts: http::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(())
            .unwrap()
            .into_parts()
            .0,
        context: context.clone(),
    };

    // Create a JSON response stream
    let graphql_response = create_sample_graphql_response();
    let json_value = serde_json::to_value(&graphql_response).unwrap();
    let json_stream = stream::once(async move { Ok(json_value) }).boxed();

    let mut core_response = CoreJsonResponse {
        extensions: Default::default(),
        responses: json_stream,
    };

    core_response.extensions.insert(Arc::new(metadata));

    let supergraph_response = core_json_response_to_supergraph_response(core_response).unwrap();

    assert_eq!(supergraph_response.response.status(), StatusCode::OK);
    assert_eq!(
        supergraph_response
            .response
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );

    // Compare specific values in context instead of direct Context comparison
    let stored_value = supergraph_response
        .context
        .get::<_, String>("response_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "response_value");

    // Verify the stream contains the expected GraphQL response
    let mut response_stream = supergraph_response.response.into_body();
    let first_response = response_stream.next().await.unwrap();
    assert_eq!(first_response, graphql_response);
}

#[tokio::test]
async fn test_supergraph_response_to_core_json_response() {
    let context = Context::new();
    context
        .insert("response_key", "response_value".to_string())
        .unwrap();

    let graphql_response = create_sample_graphql_response();
    let expected_json = serde_json::to_value(&graphql_response).unwrap();

    // Create a GraphQL response stream
    let graphql_stream = stream::once(async move { graphql_response }).boxed();

    let http_response = http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(graphql_stream)
        .unwrap();

    let supergraph_response = SupergraphResponse {
        response: http_response,
        context: context.clone(),
    };

    let core_response = supergraph_response_to_core_json_response(supergraph_response).unwrap();

    // Verify metadata is stored in extensions
    let metadata_arc = core_response
        .extensions
        .get::<Arc<ResponseMetadata>>()
        .unwrap();
    assert_eq!(metadata_arc.http_parts.status, StatusCode::OK);
    let stored_value = metadata_arc
        .context
        .get::<_, String>("response_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "response_value");

    // Verify the stream contains the expected JSON
    let mut json_stream = core_response.responses;
    let first_json = json_stream.next().await.unwrap().unwrap();
    assert_eq!(first_json, expected_json);
}

#[tokio::test]
async fn test_roundtrip_request_conversion() {
    let context = Context::new();
    context
        .insert("roundtrip_key", "roundtrip_value".to_string())
        .unwrap();

    let graphql_request = create_sample_graphql_request();

    let original_request = SupergraphRequest {
        supergraph_request: http::Request::builder()
            .method("POST")
            .uri("http://example.com/graphql")
            .header("content-type", "application/json")
            .body(graphql_request.clone())
            .unwrap(),
        context: context.clone(),
    };

    // Store original context value for comparison
    let original_value = original_request
        .context
        .get::<_, String>("roundtrip_key")
        .unwrap()
        .unwrap();

    // Convert to core and back
    let core_request = supergraph_request_to_core_json_request(original_request).unwrap();
    let converted_request = core_json_request_to_supergraph_request(core_request).unwrap();

    assert_eq!(converted_request.supergraph_request.method(), "POST");
    assert_eq!(
        converted_request.supergraph_request.uri(),
        "http://example.com/graphql"
    );
    assert_eq!(
        converted_request
            .supergraph_request
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        converted_request.supergraph_request.body(),
        &graphql_request
    );

    // Compare context values
    let final_value = converted_request
        .context
        .get::<_, String>("roundtrip_key")
        .unwrap()
        .unwrap();
    assert_eq!(original_value, final_value);
}

#[tokio::test]
async fn test_roundtrip_response_conversion() {
    let context = Context::new();
    context
        .insert(
            "roundtrip_response_key",
            "roundtrip_response_value".to_string(),
        )
        .unwrap();

    let graphql_response = create_sample_graphql_response();

    // Create a GraphQL response stream with multiple responses
    let graphql_responses = vec![graphql_response.clone(), graphql_response.clone()];
    let graphql_stream = stream::iter(graphql_responses.clone()).boxed();

    let original_response = SupergraphResponse {
        response: http::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(graphql_stream)
            .unwrap(),
        context: context.clone(),
    };

    // Store original context value for comparison
    let original_value = original_response
        .context
        .get::<_, String>("roundtrip_response_key")
        .unwrap()
        .unwrap();

    // Convert to core and back
    let core_response = supergraph_response_to_core_json_response(original_response).unwrap();
    let converted_response = core_json_response_to_supergraph_response(core_response).unwrap();

    assert_eq!(converted_response.response.status(), StatusCode::OK);
    assert_eq!(
        converted_response
            .response
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );

    // Compare context values
    let final_value = converted_response
        .context
        .get::<_, String>("roundtrip_response_key")
        .unwrap()
        .unwrap();
    assert_eq!(original_value, final_value);

    // Verify the stream contains the expected GraphQL responses
    let mut response_stream = converted_response.response.into_body();
    let mut collected_responses = Vec::new();
    while let Some(response) = response_stream.next().await {
        collected_responses.push(response);
    }
    assert_eq!(collected_responses, graphql_responses);
}

#[tokio::test]
async fn test_complex_graphql_request_conversion() {
    let context = Context::new();
    context
        .insert("complex_key", "complex_value".to_string())
        .unwrap();

    // Create a more complex GraphQL request
    let graphql_request = graphql::Request::builder()
            .query("query GetUser($id: ID!, $includeProfile: Boolean) { user(id: $id) { id name @include(if: $includeProfile) { profile { bio } } } }")
            .operation_name("GetUser")
            .variable("id", "12345")
            .variable("includeProfile", true)
            .extension("tracing", json!({"enabled": true, "level": "debug"}))
            .extension("caching", json!({"ttl": 300, "key": "user:12345"}))
            .build();

    let original_request = SupergraphRequest {
        supergraph_request: http::Request::builder()
            .method("POST")
            .uri("http://api.example.com/graphql")
            .header("authorization", "Bearer token123")
            .header("x-client-name", "test-client")
            .body(graphql_request.clone())
            .unwrap(),
        context: context.clone(),
    };

    // Round trip conversion
    let core_request = supergraph_request_to_core_json_request(original_request).unwrap();
    let converted_request = core_json_request_to_supergraph_request(core_request).unwrap();

    // Verify all properties are preserved
    assert_eq!(converted_request.supergraph_request.method(), "POST");
    assert_eq!(
        converted_request.supergraph_request.uri(),
        "http://api.example.com/graphql"
    );
    assert_eq!(
        converted_request
            .supergraph_request
            .headers()
            .get("authorization")
            .unwrap(),
        "Bearer token123"
    );
    assert_eq!(
        converted_request
            .supergraph_request
            .headers()
            .get("x-client-name")
            .unwrap(),
        "test-client"
    );
    assert_eq!(
        converted_request.supergraph_request.body(),
        &graphql_request
    );

    let stored_value = converted_request
        .context
        .get::<_, String>("complex_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "complex_value");
}
