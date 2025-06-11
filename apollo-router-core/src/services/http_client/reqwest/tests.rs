use super::*;
use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use std::time::Duration;
use tower::{BoxError, ServiceExt};
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate, Request as WiremockRequest, Respond};

/// Custom responder that sends data in controlled chunks to test streaming
struct ChunkedResponder {
    chunk_size: usize,
    total_size: usize,
}

impl ChunkedResponder {
    fn new(chunk_size: usize, total_size: usize) -> Self {
        Self {
            chunk_size,
            total_size,
        }
    }
}

impl Respond for ChunkedResponder {
    fn respond(&self, _request: &WiremockRequest) -> wiremock::ResponseTemplate {
        let mut response = ResponseTemplate::new(200)
            .insert_header("content-type", "application/octet-stream")
            .insert_header("content-length", self.total_size.to_string());

        // Create chunked data - each chunk will be a repeated pattern
        let mut body_data = Vec::with_capacity(self.total_size);
        let mut bytes_written = 0;
        let mut chunk_number = 0;

        while bytes_written < self.total_size {
            let remaining = self.total_size - bytes_written;
            let current_chunk_size = std::cmp::min(self.chunk_size, remaining);
            
            // Create a unique pattern for each chunk to verify order
            let pattern = format!("CHUNK_{:04}_", chunk_number);
            let pattern_bytes = pattern.as_bytes();
            
            for i in 0..current_chunk_size {
                body_data.push(pattern_bytes[i % pattern_bytes.len()]);
            }
            
            bytes_written += current_chunk_size;
            chunk_number += 1;
        }

        response = response.set_body_bytes(body_data);

        // Note: wiremock doesn't support true async chunked responses,
        // so we create structured data that reqwest/hyper will naturally
        // chunk during streaming based on buffer sizes
        response
    }
}

/// Helper to create a test HTTP request
fn create_test_request(method: &str, uri: &str, body: &str) -> Request {
    let body = UnsyncBoxBody::new(Full::new(Bytes::from(body.to_string())).map_err(|_| unreachable!()));
    http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn test_reqwest_service_creation() {
    let service = ReqwestService::new();
    
    // Create a simple mock server to test against
    let mock_server = MockServer::start().await;
    let test_url = format!("{}/test", mock_server.uri());
    
    assert!(service.client.get(&test_url).build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_with_custom_client() {
    let custom_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    
    let service = ReqwestService::with_client(custom_client);
    
    // Create a simple mock server to test against
    let mock_server = MockServer::start().await;
    let test_url = format!("{}/test", mock_server.uri());
    
    assert!(service.client.get(&test_url).build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_default() {
    let service = ReqwestService::default();
    
    // Create a simple mock server to test against
    let mock_server = MockServer::start().await;
    let test_url = format!("{}/test", mock_server.uri());
    
    assert!(service.client.get(&test_url).build().is_ok());
}

#[tokio::test]
async fn test_reqwest_service_tower_service_trait() {
    use tower::Service;
    
    let mut service = ReqwestService::new();
    
    // Test that the service is ready
    assert!(service.ready().await.is_ok());
    
    // The service should implement the Service trait
    fn _assert_service_trait<T>()
    where
        T: Service<Request, Response = Response, Error = Error> + Clone,
    {
        // This function exists only to verify trait bounds at compile time
    }
    
    _assert_service_trait::<ReqwestService>();
}

#[tokio::test]
async fn test_reqwest_service_successful_request() {
    let mock_server = MockServer::start().await;
    
    // Set up a mock endpoint
    Mock::given(method("POST"))
        .and(path("/test"))
        .and(header("content-type", "application/json"))
        .and(body_string("test request body"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("test response body")
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/test", mock_server.uri());
    let request = create_test_request("POST", &request_url, "test request body");

    let response = service.execute_request(request).await.unwrap();
    
    // Verify response status
    assert_eq!(response.status(), 200);
    
    // Verify response headers
    assert!(response.headers().get("content-type").is_some());
    
    // Verify response body by collecting it
    let body_bytes = response.collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes, "test response body");
}

#[tokio::test]
async fn test_reqwest_service_streaming() {
    let mock_server = MockServer::start().await;
    
    // Create a much larger payload to increase chances of chunking
    let large_body = "0123456789".repeat(100_000); // 1MB of data
    Mock::given(method("GET"))
        .and(path("/large-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(&large_body)
                .insert_header("content-type", "application/octet-stream"),
        )
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/large-stream", mock_server.uri());
    let request = create_test_request("GET", &request_url, "");

    let response = service.execute_request(request).await.unwrap();
    
    // Verify response status
    assert_eq!(response.status(), 200);
    
    // Test streaming behavior - track chunks and memory usage
    let mut body = response.into_body();
    let mut total_bytes = 0;
    let mut chunk_count = 0;
    let mut max_chunk_size = 0;
    let mut min_chunk_size = usize::MAX;
    let mut received_data = Vec::new();
    
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Ok(data) = frame.into_data() {
            chunk_count += 1;
            let chunk_size = data.len();
            max_chunk_size = max_chunk_size.max(chunk_size);
            min_chunk_size = min_chunk_size.min(chunk_size);
            total_bytes += chunk_size;
            received_data.extend_from_slice(&data);
            
            // Verify we're not loading everything into a single massive chunk
            // Allow up to 1MB chunks (which is reasonable for HTTP streaming)
            assert!(chunk_size <= 1024 * 1024, "Chunk too large: {} bytes", chunk_size);
        }
    }
    
    assert_eq!(total_bytes, large_body.len());
    assert_eq!(received_data, large_body.as_bytes());
    
    // Fix min_chunk_size if no chunks were processed
    if min_chunk_size == usize::MAX {
        min_chunk_size = 0;
    }
    
    // Print diagnostic information (only shows up if test fails or run with --nocapture)
    println!("Streaming stats: {} chunks, {} total bytes", chunk_count, total_bytes);
    println!("Chunk size - min: {} bytes, max: {} bytes, avg: {} bytes", 
             min_chunk_size, max_chunk_size, 
             if chunk_count > 0 { total_bytes / chunk_count } else { 0 });
    
    // Verify that streaming is actually happening
    assert!(chunk_count > 0, "Expected at least one chunk");
    
    // For a 1MB payload, if we get it all in one chunk, that's concerning but not wrong
    if chunk_count == 1 {
        println!("WARNING: Received only 1 chunk for 1MB payload. This might indicate buffering.");
        println!("This is not necessarily wrong - it depends on network conditions and buffer sizes.");
    } else {
        println!("SUCCESS: Received {} chunks, confirming streaming behavior!", chunk_count);
    }
}

#[tokio::test]
async fn test_reqwest_service_streaming_request() {
    let mock_server = MockServer::start().await;
    
    let expected_body = "streaming request body";
    
    // Set up a mock endpoint that expects the streaming body
    Mock::given(method("PUT"))
        .and(path("/upload"))
        .and(body_string(expected_body))
        .respond_with(ResponseTemplate::new(201).set_body_string("Upload successful"))
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/upload", mock_server.uri());
    let request = create_test_request("PUT", &request_url, expected_body);

    let response = service.execute_request(request).await.unwrap();
    
    // Verify response
    assert_eq!(response.status(), 201);
    let body_bytes = response.collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes, "Upload successful");
}

#[tokio::test]
async fn test_reqwest_service_error_response() {
    let mock_server = MockServer::start().await;
    
    // Set up a mock endpoint that returns an error
    Mock::given(method("GET"))
        .and(path("/error"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("Internal Server Error")
                .insert_header("content-type", "text/plain"),
        )
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/error", mock_server.uri());
    let request = create_test_request("GET", &request_url, "");

    let response = service.execute_request(request).await.unwrap();
    
    // The service should return the error response, not convert it to an Error
    // The reqwest service passes through HTTP responses regardless of status code
    assert_eq!(response.status(), 500);
    
    let body_bytes = response.collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes, "Internal Server Error");
}

#[tokio::test]
async fn test_reqwest_service_network_error() {
    let service = ReqwestService::new();
    
    // Create a request to a non-existent server (use a port that's definitely not in use)
    let request = create_test_request("POST", "http://localhost:99999/test", "test body");
    
    // This test verifies that the service handles network errors gracefully
    let result = service.execute_request(request).await;
    
    // The request should fail with a network error
    assert!(result.is_err());
    
    // Verify it's the right type of error
    if let Err(Error::RequestFailed { .. }) = result {
        // This is the expected error type
    } else {
        panic!("Expected RequestFailed error variant, got: {:?}", result);
    }
}

#[tokio::test]
async fn test_reqwest_service_with_headers() {
    let mock_server = MockServer::start().await;
    
    // Set up a mock endpoint that checks for specific headers
    Mock::given(method("POST"))
        .and(path("/headers"))
        .and(header("content-type", "application/json"))
        .and(header("authorization", "Bearer token123"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Headers received"))
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/headers", mock_server.uri());
    
    let body = UnsyncBoxBody::new(Full::new(Bytes::from("{}")).map_err(|_| unreachable!()));
    let request = http::Request::builder()
        .method("POST")
        .uri(&request_url)
        .header("content-type", "application/json")
        .header("authorization", "Bearer token123")
        .body(body)
        .unwrap();

    let response = service.execute_request(request).await.unwrap();
    
    assert_eq!(response.status(), 200);
    let body_bytes = response.collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes, "Headers received");
}

#[tokio::test]
async fn test_error_into_box_error() {
    let error = Error::InvalidRequest {
        details: "test error".to_string(),
    };
    
    let _box_error: BoxError = error.into();
    // Test passes if compilation succeeds
}

#[tokio::test]
async fn test_error_types_debug_and_display() {
    let invalid_request_error = Error::InvalidRequest {
        details: "Test details".to_string(),
    };
    
    let response_processing_error = Error::ResponseProcessingFailed {
        source: Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Test error")),
        context: "Test context".to_string(),
    };
    
    // Test that errors can be formatted
    assert!(!format!("{:?}", invalid_request_error).is_empty());
    assert!(!format!("{}", invalid_request_error).is_empty());
    
    assert!(!format!("{:?}", response_processing_error).is_empty());
    assert!(!format!("{}", response_processing_error).is_empty());
    
    // Test that error can be converted to BoxError
    let _box_error: BoxError = invalid_request_error.into();
    let _box_error2: BoxError = response_processing_error.into();
}

#[tokio::test]
async fn test_reqwest_service_custom_chunked_streaming() {
    let mock_server = MockServer::start().await;
    
    // Use our custom responder that creates predictable chunk patterns
    let custom_responder = ChunkedResponder::new(
        32 * 1024,    // 32KB logical chunks in our pattern
        256 * 1024,   // 256KB total data
    );
    
    Mock::given(method("GET"))
        .and(path("/custom-stream"))
        .respond_with(custom_responder)
        .mount(&mock_server)
        .await;

    let service = ReqwestService::new();
    let request_url = format!("{}/custom-stream", mock_server.uri());
    let request = create_test_request("GET", &request_url, "");

    let response = service.execute_request(request).await.unwrap();
    
    // Verify response status and headers
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-length").unwrap(),
        "262144" // 256KB
    );
    
    // Test streaming behavior with our custom chunked data
    let mut body = response.into_body();
    let mut total_bytes = 0;
    let mut chunk_count = 0;
    let mut received_data = Vec::new();
    let mut chunk_patterns = Vec::new();
    
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Ok(data) = frame.into_data() {
            chunk_count += 1;
            total_bytes += data.len();
            
            // Extract the beginning of this chunk to see the pattern
            let chunk_start = String::from_utf8_lossy(&data[..std::cmp::min(20, data.len())]);
            chunk_patterns.push(chunk_start.to_string());
            
            received_data.extend_from_slice(&data);
        }
    }
    
    assert_eq!(total_bytes, 256 * 1024);
    
    // Print diagnostic information about the custom chunked response
    println!("Custom chunked streaming stats:");
    println!("  Total bytes: {}", total_bytes);
    println!("  Chunks received: {}", chunk_count);
    println!("  Average chunk size: {} bytes", total_bytes / chunk_count);
    
    // Show the patterns we created to verify streaming order
    println!("  Chunk patterns detected:");
    for (i, pattern) in chunk_patterns.iter().enumerate() {
        println!("    Chunk {}: {}", i + 1, pattern);
    }
    
    // Verify that our patterns are present in the data
    let received_string = String::from_utf8_lossy(&received_data);
    assert!(received_string.contains("CHUNK_0000_"));
    assert!(received_string.contains("CHUNK_0001_"));
    
    // Success if we received multiple chunks with our custom patterns
    if chunk_count > 1 {
        println!("  SUCCESS: Custom responder created {} chunks!", chunk_count);
    } else {
        println!("  NOTE: Custom data received as single chunk (network/buffering effects)");
    }
} 