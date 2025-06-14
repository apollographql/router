use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;

use super::*;
use crate::Context;

#[tokio::test]
async fn test_core_request_to_router_request_conversion() {
    // Create a router core request
    let body_data = "test request body";
    let core_body = Full::new(Bytes::from(body_data))
        .map_err(|never| -> BoxError { match never {} })
        .boxed_unsync();

    let mut core_request = http::Request::builder()
        .method("POST")
        .uri("https://example.com/graphql")
        .header("content-type", "application/json")
        .body(core_body)
        .unwrap();

    // Add required metadata to extensions
    let metadata = RequestMetadata {
        context: Context::new(),
    };
    core_request.extensions_mut().insert(Arc::new(metadata));

    // Convert to router request using async function
    let router_request = core_request_to_router_request(core_request).unwrap();

    // Verify the conversion preserved headers and method
    assert_eq!(router_request.http_request.method(), "POST");
    assert_eq!(
        router_request.http_request.uri(),
        "https://example.com/graphql"
    );
    assert_eq!(
        router_request
            .http_request
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );

    // Verify body can be read (stream is preserved)
    let body_bytes = http_body_util::BodyExt::collect(router_request.http_request.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_router_request_to_core_request_conversion() {
    // Create a regular router request
    let body_data = "test request body";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_request = http::Request::builder()
        .method("GET")
        .uri("https://api.example.com/data")
        .header("authorization", "Bearer token123")
        .body(router_body)
        .unwrap();

    let router_request = RouterHttpRequest {
        http_request,
        context: Context::new(),
    };

    // Convert to core request using async function
    let core_request = router_request_to_core_request(router_request).unwrap();

    // Verify the conversion preserved headers and method
    assert_eq!(core_request.method(), "GET");
    assert_eq!(core_request.uri(), "https://api.example.com/data");
    assert_eq!(
        core_request.headers().get("authorization").unwrap(),
        "Bearer token123"
    );

    // Verify body can be read (stream is preserved)
    let body_bytes = http_body_util::BodyExt::collect(core_request.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_core_response_to_router_response_conversion() {
    // Create a router core response
    let body_data = "test response body";
    let core_body = Full::new(Bytes::from(body_data))
        .map_err(|never| -> BoxError { match never {} })
        .boxed_unsync();

    let mut core_response = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("cache-control", "no-cache")
        .body(core_body)
        .unwrap();

    // Add required metadata to extensions
    let metadata = ResponseMetadata {
        context: Context::new(),
    };
    core_response.extensions_mut().insert(Arc::new(metadata));

    // Convert to router response using async function
    let router_response = core_response_to_router_response(core_response).unwrap();

    // Verify the conversion preserved status and headers
    assert_eq!(router_response.http_response.status(), 200);
    assert_eq!(
        router_response
            .http_response
            .headers()
            .get("content-type")
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        router_response
            .http_response
            .headers()
            .get("cache-control")
            .unwrap(),
        "no-cache"
    );

    // Verify body can be read (stream is preserved)
    let body_bytes = http_body_util::BodyExt::collect(router_response.http_response.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_router_response_to_core_response_conversion() {
    // Create a regular router response
    let body_data = "test response body";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_response = http::Response::builder()
        .status(404)
        .header("content-length", "18")
        .body(router_body)
        .unwrap();

    let router_response = RouterHttpResponse {
        http_response,
        context: Context::new(),
    };

    // Convert to core response using async function
    let core_response = router_response_to_core_response(router_response).unwrap();

    // Verify the conversion preserved status and headers
    assert_eq!(core_response.status(), 404);
    assert_eq!(core_response.headers().get("content-length").unwrap(), "18");

    // Verify body can be read (stream is preserved)
    let body_bytes = http_body_util::BodyExt::collect(core_response.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_round_trip_request_conversion() {
    // Create original router request (starting from router side to avoid metadata setup)
    let body_data = "round trip test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_request = http::Request::builder()
        .method("PUT")
        .uri("https://example.com/update")
        .header("x-test-header", "test-value")
        .body(router_body)
        .unwrap();

    let original_request = RouterHttpRequest {
        http_request,
        context: Context::new(),
    };

    // Round trip: Router -> Core -> Router
    let core_request = router_request_to_core_request(original_request).unwrap();
    let final_request = core_request_to_router_request(core_request).unwrap();

    // Verify round trip preserved all properties
    assert_eq!(final_request.http_request.method(), "PUT");
    assert_eq!(
        final_request.http_request.uri(),
        "https://example.com/update"
    );
    assert_eq!(
        final_request
            .http_request
            .headers()
            .get("x-test-header")
            .unwrap(),
        "test-value"
    );

    // Verify body integrity
    let body_bytes = http_body_util::BodyExt::collect(final_request.http_request.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_round_trip_response_conversion() {
    // Create original router response
    let body_data = "round trip response";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_response = http::Response::builder()
        .status(201)
        .header("location", "/new-resource")
        .body(router_body)
        .unwrap();

    let original_response = RouterHttpResponse {
        http_response,
        context: Context::new(),
    };

    // Round trip: Router -> Core -> Router
    let core_response = router_response_to_core_response(original_response).unwrap();
    let final_response = core_response_to_router_response(core_response).unwrap();

    // Verify round trip preserved all properties
    assert_eq!(final_response.http_response.status(), 201);
    assert_eq!(
        final_response
            .http_response
            .headers()
            .get("location")
            .unwrap(),
        "/new-resource"
    );

    // Verify body integrity
    let body_bytes = http_body_util::BodyExt::collect(final_response.http_response.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body_bytes, Bytes::from(body_data));
}

#[tokio::test]
async fn test_context_preservation_in_request_round_trip() {
    // Create a router request with custom context data
    let body_data = "context test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_request = http::Request::builder()
        .method("POST")
        .uri("https://example.com/context")
        .body(router_body)
        .unwrap();

    let context = Context::new();
    context
        .insert("test_key", "test_value".to_string())
        .unwrap();
    context.insert("number_key", 42i32).unwrap();

    let original_request = RouterHttpRequest {
        http_request,
        context,
    };

    // Store reference values for comparison
    let original_test_value = original_request
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    let original_number_value = original_request
        .context
        .get::<_, i32>("number_key")
        .unwrap()
        .unwrap();

    // Round trip: Router -> Core -> Router
    let core_request = router_request_to_core_request(original_request).unwrap();
    let final_request = core_request_to_router_request(core_request).unwrap();

    // Verify context data is preserved
    let final_test_value = final_request
        .context
        .get::<_, String>("test_key")
        .unwrap()
        .unwrap();
    let final_number_value = final_request
        .context
        .get::<_, i32>("number_key")
        .unwrap()
        .unwrap();

    assert_eq!(original_test_value, final_test_value);
    assert_eq!(original_number_value, final_number_value);

    // Verify HTTP properties are still preserved
    assert_eq!(final_request.http_request.method(), "POST");
    assert_eq!(
        final_request.http_request.uri(),
        "https://example.com/context"
    );
}

#[tokio::test]
async fn test_context_preservation_in_response_round_trip() {
    // Create a router response with custom context data
    let body_data = "response context test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let http_response = http::Response::builder()
        .status(200)
        .header("x-custom-header", "custom-value")
        .body(router_body)
        .unwrap();

    let context = Context::new();
    context
        .insert("response_data", "important_info".to_string())
        .unwrap();
    context.insert("response_id", 12345u64).unwrap();

    let original_response = RouterHttpResponse {
        http_response,
        context,
    };

    // Store reference values for comparison
    let original_data = original_response
        .context
        .get::<_, String>("response_data")
        .unwrap()
        .unwrap();
    let original_id = original_response
        .context
        .get::<_, u64>("response_id")
        .unwrap()
        .unwrap();

    // Round trip: Router -> Core -> Router
    let core_response = router_response_to_core_response(original_response).unwrap();
    let final_response = core_response_to_router_response(core_response).unwrap();

    // Verify context data is preserved
    let final_data = final_response
        .context
        .get::<_, String>("response_data")
        .unwrap()
        .unwrap();
    let final_id = final_response
        .context
        .get::<_, u64>("response_id")
        .unwrap()
        .unwrap();

    assert_eq!(original_data, final_data);
    assert_eq!(original_id, final_id);

    // Verify HTTP properties are still preserved
    assert_eq!(final_response.http_response.status(), 200);
    assert_eq!(
        final_response
            .http_response
            .headers()
            .get("x-custom-header")
            .unwrap(),
        "custom-value"
    );
}

#[tokio::test]
async fn test_http_extensions_preservation_in_request_round_trip() {
    // Create a router request with HTTP extensions (starting from router side)
    let body_data = "extensions test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let mut http_request = http::Request::builder()
        .method("POST")
        .uri("https://example.com/extensions")
        .body(router_body)
        .unwrap();

    // Add custom data to HTTP extensions
    http_request
        .extensions_mut()
        .insert("http_ext_string".to_string());
    http_request.extensions_mut().insert(999u32);
    http_request.extensions_mut().insert(vec![1, 2, 3, 4, 5]);

    let original_request = RouterHttpRequest {
        http_request,
        context: Context::new(),
    };

    // Store reference values for comparison
    let original_string = original_request
        .http_request
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let original_number = *original_request
        .http_request
        .extensions()
        .get::<u32>()
        .unwrap();
    let original_vec = original_request
        .http_request
        .extensions()
        .get::<Vec<i32>>()
        .unwrap()
        .clone();

    // Round trip: Router -> Core -> Router
    let core_request = router_request_to_core_request(original_request).unwrap();
    let final_request = core_request_to_router_request(core_request).unwrap();

    // Verify HTTP extensions are preserved
    let final_string = final_request
        .http_request
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let final_number = *final_request
        .http_request
        .extensions()
        .get::<u32>()
        .unwrap();
    let final_vec = final_request
        .http_request
        .extensions()
        .get::<Vec<i32>>()
        .unwrap()
        .clone();

    assert_eq!(original_string, final_string);
    assert_eq!(original_number, final_number);
    assert_eq!(original_vec, final_vec);

    // Verify HTTP properties are still preserved
    assert_eq!(final_request.http_request.method(), "POST");
    assert_eq!(
        final_request.http_request.uri(),
        "https://example.com/extensions"
    );
}

#[tokio::test]
async fn test_http_extensions_preservation_in_response_round_trip() {
    // Create a router response with HTTP extensions
    let body_data = "response extensions test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let mut http_response = http::Response::builder()
        .status(201)
        .header("x-test", "value")
        .body(router_body)
        .unwrap();

    // Add custom data to HTTP extensions
    http_response
        .extensions_mut()
        .insert("response_ext_data".to_string());
    http_response.extensions_mut().insert(777u64);
    #[derive(Debug, Clone, PartialEq)]
    struct CustomData {
        name: String,
        value: i32,
    }
    let custom_data = CustomData {
        name: "test".to_string(),
        value: 123,
    };
    http_response.extensions_mut().insert(custom_data.clone());

    let original_response = RouterHttpResponse {
        http_response,
        context: Context::new(),
    };

    // Store reference values for comparison
    let original_string = original_response
        .http_response
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let original_number = *original_response
        .http_response
        .extensions()
        .get::<u64>()
        .unwrap();
    let original_custom = original_response
        .http_response
        .extensions()
        .get::<CustomData>()
        .unwrap()
        .clone();

    // Round trip: Router -> Core -> Router
    let core_response = router_response_to_core_response(original_response).unwrap();
    let final_response = core_response_to_router_response(core_response).unwrap();

    // Verify HTTP extensions are preserved
    let final_string = final_response
        .http_response
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let final_number = *final_response
        .http_response
        .extensions()
        .get::<u64>()
        .unwrap();
    let final_custom = final_response
        .http_response
        .extensions()
        .get::<CustomData>()
        .unwrap()
        .clone();

    assert_eq!(original_string, final_string);
    assert_eq!(original_number, final_number);
    assert_eq!(original_custom, final_custom);

    // Verify HTTP properties are still preserved
    assert_eq!(final_response.http_response.status(), 201);
    assert_eq!(
        final_response
            .http_response
            .headers()
            .get("x-test")
            .unwrap(),
        "value"
    );
}

#[tokio::test]
async fn test_mixed_extensions_and_context_preservation() {
    // Create a router request with both HTTP extensions and router context
    let body_data = "mixed preservation test";
    let router_body = Full::new(Bytes::from(body_data))
        .map_err(|never| axum::Error::new(never))
        .boxed_unsync();

    let mut http_request = http::Request::builder()
        .method("PUT")
        .uri("https://example.com/mixed")
        .body(router_body)
        .unwrap();

    // Add HTTP extensions
    http_request
        .extensions_mut()
        .insert("http_data".to_string());
    http_request.extensions_mut().insert(555u16);

    // Create router context
    let context = Context::new();
    context
        .insert("router_data", "context_value".to_string())
        .unwrap();
    context.insert("router_number", 777i64).unwrap();

    let original_request = RouterHttpRequest {
        http_request,
        context,
    };

    // Store reference values for comparison
    let original_http_string = original_request
        .http_request
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let original_http_number = *original_request
        .http_request
        .extensions()
        .get::<u16>()
        .unwrap();
    let original_context_string = original_request
        .context
        .get::<_, String>("router_data")
        .unwrap()
        .unwrap();
    let original_context_number = original_request
        .context
        .get::<_, i64>("router_number")
        .unwrap()
        .unwrap();

    // Round trip: Router -> Core -> Router
    let core_request = router_request_to_core_request(original_request).unwrap();
    let final_request = core_request_to_router_request(core_request).unwrap();

    // Verify HTTP extensions are preserved
    let final_http_string = final_request
        .http_request
        .extensions()
        .get::<String>()
        .unwrap()
        .clone();
    let final_http_number = *final_request
        .http_request
        .extensions()
        .get::<u16>()
        .unwrap();

    assert_eq!(original_http_string, final_http_string);
    assert_eq!(original_http_number, final_http_number);

    // Verify router context is preserved
    let final_context_string = final_request
        .context
        .get::<_, String>("router_data")
        .unwrap()
        .unwrap();
    let final_context_number = final_request
        .context
        .get::<_, i64>("router_number")
        .unwrap()
        .unwrap();

    assert_eq!(original_context_string, final_context_string);
    assert_eq!(original_context_number, final_context_number);

    // Verify HTTP properties are still preserved
    assert_eq!(final_request.http_request.method(), "PUT");
    assert_eq!(
        final_request.http_request.uri(),
        "https://example.com/mixed"
    );
}
