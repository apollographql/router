use std::sync::Arc;

use super::{
    RequestMetadata, ResponseMetadata, core_request_to_router_request,
    core_response_to_router_response, router_request_to_core_request,
    router_response_to_core_response,
};
use crate::Context;
use crate::services::router::body;
use crate::services::router::{Request as RouterRequest, Response as RouterResponse};

use bytes::Bytes;
use http::StatusCode;
use http_body_util::BodyExt;

#[tokio::test]
async fn test_core_request_to_router_request() {
    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();

    let metadata = RequestMetadata {
        context: context.clone(),
    };

    let body = http_body_util::Full::new(Bytes::from("test body"))
        .map_err(|never| -> tower::BoxError { match never {} })
        .boxed_unsync();

    let mut core_request = http::Request::builder()
        .method("POST")
        .uri("http://example.com/graphql")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    core_request.extensions_mut().insert(Arc::new(metadata));

    let router_request = core_request_to_router_request(core_request).unwrap();

    assert_eq!(router_request.router_request.method(), "POST");
    assert_eq!(
        router_request.router_request.uri(),
        "http://example.com/graphql"
    );
    assert_eq!(
        router_request
            .router_request
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );

    // Compare specific values in context instead of direct Context comparison
    let stored_value = router_request
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "test_value");
}

#[tokio::test]
async fn test_router_request_to_core_request() {
    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();

    let body = body::from_bytes("test body");

    let http_request = http::Request::builder()
        .method("POST")
        .uri("http://example.com/graphql")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let router_request = RouterRequest {
        router_request: http_request,
        context: context.clone(),
    };

    let core_request = router_request_to_core_request(router_request).unwrap();

    assert_eq!(core_request.method(), "POST");
    assert_eq!(core_request.uri(), "http://example.com/graphql");
    assert_eq!(
        core_request.headers().get("content-type").unwrap(),
        "application/json"
    );

    // Verify metadata is stored in extensions
    let metadata_arc = core_request
        .extensions()
        .get::<Arc<RequestMetadata>>()
        .unwrap();
    let stored_value = metadata_arc
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "test_value");
}

#[tokio::test]
async fn test_core_response_to_router_response() {
    let context = Context::new();
    context
        .insert("response_key", "response_value".to_string())
        .unwrap();

    let metadata = ResponseMetadata {
        context: context.clone(),
    };

    let body = http_body_util::Full::new(Bytes::from("test response"))
        .map_err(|never| -> tower::BoxError { match never {} })
        .boxed_unsync();

    let mut core_response = http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    core_response.extensions_mut().insert(Arc::new(metadata));

    let router_response = core_response_to_router_response(core_response).unwrap();

    assert_eq!(router_response.response.status(), StatusCode::OK);
    assert_eq!(
        router_response
            .response
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );

    // Compare specific values in context instead of direct Context comparison
    let stored_value = router_response
        .context
        .get::<_, String>("response_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "response_value");
}

#[tokio::test]
async fn test_router_response_to_core_response() {
    let context = Context::new();
    context
        .insert("response_key", "response_value".to_string())
        .unwrap();

    let body = body::from_bytes("test response");

    let http_response = http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let router_response = RouterResponse {
        response: http_response,
        context: context.clone(),
    };

    let core_response = router_response_to_core_response(router_response).unwrap();

    assert_eq!(core_response.status(), StatusCode::OK);
    assert_eq!(
        core_response.headers().get("content-type").unwrap(),
        "application/json"
    );

    // Verify metadata is stored in extensions
    let metadata_arc = core_response
        .extensions()
        .get::<Arc<ResponseMetadata>>()
        .unwrap();
    let stored_value = metadata_arc
        .context
        .get::<_, String>("response_key")
        .unwrap()
        .unwrap();
    assert_eq!(stored_value, "response_value");
}

#[tokio::test]
async fn test_roundtrip_request_conversion() {
    let context = Context::new();
    context
        .insert("roundtrip_key", "roundtrip_value".to_string())
        .unwrap();

    let body = body::from_bytes("test body");

    let original_request = RouterRequest {
        router_request: http::Request::builder()
            .method("POST")
            .uri("http://example.com/graphql")
            .header("content-type", "application/json")
            .body(body)
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
    let core_request = router_request_to_core_request(original_request).unwrap();
    let converted_request = core_request_to_router_request(core_request).unwrap();

    assert_eq!(converted_request.router_request.method(), "POST");
    assert_eq!(
        converted_request.router_request.uri(),
        "http://example.com/graphql"
    );
    assert_eq!(
        converted_request
            .router_request
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
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

    let body = body::from_bytes("test response");

    let original_response = RouterResponse {
        response: http::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(body)
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
    let core_response = router_response_to_core_response(original_response).unwrap();
    let converted_response = core_response_to_router_response(core_response).unwrap();

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
}
