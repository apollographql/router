use std::sync::Arc;
use std::task::{Context, Poll};

use tower::{BoxError, Service};
use tower::load_shed::error::Overloaded;

use super::*;

// Test request and response types
#[derive(Clone, Debug, PartialEq)]
struct TestRequest {
    query: String,
    id: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct TestResponse {
    data: String,
}

// Test error types
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Parse error: {message}")]
struct ParseError {
    message: String,
}

// Mock service for testing
#[derive(Clone)]
struct MockService {
    responses: Arc<std::sync::Mutex<Vec<Result<TestResponse, BoxError>>>>,
    call_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl MockService {
    fn new(responses: Vec<Result<TestResponse, BoxError>>) -> Self {
        Self {
            responses: Arc::new(std::sync::Mutex::new(responses)),
            call_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Service<TestRequest> for MockService {
    type Response = TestResponse;
    type Error = BoxError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: TestRequest) -> Self::Future {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let responses = self.responses.clone();
        Box::pin(async move {
            let mut responses = responses.lock().unwrap();
            if !responses.is_empty() {
                responses.remove(0)
            } else {
                Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "No more responses",
                )) as BoxError)
            }
        })
    }
}

#[tokio::test]
async fn test_cache_successful_responses() {
    // Create a service that returns success on multiple calls
    let mock_service = MockService::new(vec![
        Ok(TestResponse {
            data: "success".to_string(),
        }),
        Ok(TestResponse {
            data: "second_call_should_not_happen".to_string(),
        }),
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Should not be called",
        )) as BoxError),
    ]);

    // Create cache layer with key extraction based on query string
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |_err: &ArcError| false, // Don't cache any errors for this test
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    let request = TestRequest {
        query: "query { user }".to_string(),
        id: 1,
    };

    // First call should hit the service
    let response1 = cached_service.call(request.clone()).await.unwrap();

    assert_eq!(response1.data, "success");
    assert_eq!(mock_service.call_count(), 1);

    // Second call with same query should hit cache
    let response2 = cached_service.call(request.clone()).await.unwrap();
    assert_eq!(response2.data, "success"); // Should be the same cached response
    assert_eq!(mock_service.call_count(), 1); // Service not called again

    // Different query should hit service
    let different_request = TestRequest {
        query: "query { posts }".to_string(),
        id: 2,
    };

    let response3 = cached_service.call(different_request).await.unwrap();
    assert_eq!(response3.data, "second_call_should_not_happen");
    assert_eq!(mock_service.call_count(), 2);
}

#[tokio::test]
async fn test_cache_specific_error_types() {
    // Create a service that returns a ParseError
    let parse_error = ParseError {
        message: "Invalid syntax".to_string(),
    };
    let mock_service = MockService::new(vec![
        Err(Box::new(parse_error.clone()) as BoxError),
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Should not be called",
        )) as BoxError),
    ]);

    // Create cache layer that caches ParseError but not other errors
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    let request = TestRequest {
        query: "invalid query".to_string(),
        id: 1,
    };

    // First call should hit the service and cache the error
    let error1 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");

    assert_eq!(error1.to_string(), "Parse error: Invalid syntax");
    assert_eq!(mock_service.call_count(), 1);

    // Second call with same query should hit cache and return cached error
    let error2 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    assert_eq!(error2.to_string(), "Parse error: Invalid syntax");
    assert_eq!(mock_service.call_count(), 1); // Service not called again
}

#[tokio::test]
async fn test_custom_key_extraction() {
    // Create a service that returns success
    let mock_service = MockService::new(vec![
        Ok(TestResponse {
            data: "response1".to_string(),
        }),
        Ok(TestResponse {
            data: "response2".to_string(),
        }),
    ]);

    // Create cache layer with key extraction based on ID instead of query
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.id.to_string(),
        |_err: &ArcError| false, // Don't cache errors
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    let request1 = TestRequest {
        query: "query { user }".to_string(),
        id: 1,
    };

    let request2 = TestRequest {
        query: "query { posts }".to_string(), // Different query
        id: 1,                                // Same ID
    };

    // First call
    let response1 = cached_service.call(request1.clone()).await.unwrap();
    assert_eq!(response1.data, "response1");
    assert_eq!(mock_service.call_count(), 1);

    // Second call with different query but same ID should hit cache
    let response2 = cached_service.call(request2.clone()).await.unwrap();
    assert_eq!(response2.data, "response1"); // Same cached response
    assert_eq!(mock_service.call_count(), 1); // Service not called again
}

#[tokio::test]
async fn test_cache_capacity_limit() {
    // Create a service that always returns success
    let mock_service = MockService::new(vec![
        Ok(TestResponse {
            data: "response1".to_string(),
        }),
        Ok(TestResponse {
            data: "response2".to_string(),
        }),
        Ok(TestResponse {
            data: "response3".to_string(),
        }),
        Ok(TestResponse {
            data: "response4".to_string(),
        }),
        Ok(TestResponse {
            data: "response5".to_string(),
        }),
    ]);

    // Create cache layer with capacity of 2
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        2,
        |req: &TestRequest| req.id.to_string(),
        |_err: &ArcError| false, // Don't cache errors
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    // Make 3 requests with different IDs
    for i in 1..=3 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };
        let response = cached_service.call(request).await.unwrap();
        assert_eq!(response.data, format!("response{}", i));
    }

    assert_eq!(mock_service.call_count(), 3);

    // Now make more requests than cache capacity to force evictions
    for i in 4..=5 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };
        let response = cached_service.call(request).await.unwrap();
        assert_eq!(response.data, format!("response{}", i));
    }

    // At this point, we've made 5 total calls
    assert_eq!(mock_service.call_count(), 5);

    // The cache should have evicted some entries due to capacity limits
    // We can't predict exactly which ones, but we can test that the cache
    // is working by making requests to all IDs and seeing that at least
    // some hit the cache (causing fewer than 5 additional service calls)
    let call_count_before = mock_service.call_count();

    for i in 1..=5 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };
        // These calls might hit cache or service, depending on eviction
        let _response = cached_service.call(request).await;
    }

    let call_count_after = mock_service.call_count();
    let additional_calls = call_count_after - call_count_before;

    // Due to cache capacity of 2, we should have fewer than 5 additional calls
    // (at least 2-3 entries should be cached)
    assert!(
        additional_calls >= 3,
        "Expected at least 3 cache misses due to eviction, got {}",
        additional_calls
    );
    assert!(
        additional_calls < 5,
        "Expected fewer than 5 cache misses, got {} (cache not working)",
        additional_calls
    );
}

#[tokio::test]
async fn test_query_parse_cache_convenience_function() {
    // Test the convenience function for query parsing
    let mock_service = MockService::new(vec![Err(Box::new(ParseError {
        message: "syntax error".to_string(),
    }) as BoxError)]);

    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = query_parse_cache(
        10,
        |req: &TestRequest| req.query.clone(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    let request = TestRequest {
        query: "invalid query".to_string(),
        id: 1,
    };

    // First call should cache the parse error
    let _error1 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    assert_eq!(mock_service.call_count(), 1);

    // Second call should return cached error
    let _error2 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    assert_eq!(mock_service.call_count(), 1); // Not called again
}

#[tokio::test]
async fn test_response_and_error_conversion_to_arc() {
    // Test that responses and errors are properly converted to Arc internally
    let original_response = TestResponse {
        data: "test".to_string(),
    };
    let original_error = ParseError {
        message: "test error".to_string(),
    };

    let mock_service = MockService::new(vec![
        Ok(original_response.clone()),
        Err(Box::new(original_error.clone()) as BoxError),
    ]);

    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.id.to_string(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    // Test successful response caching
    let request1 = TestRequest {
        query: "query".to_string(),
        id: 1,
    };
    let response = cached_service.call(request1.clone()).await.unwrap();
    assert_eq!(*response, original_response);

    // Call again to ensure it comes from cache (Arc cloning)
    let cached_response = cached_service.call(request1).await.unwrap();
    assert_eq!(*cached_response, original_response);

    // Test error caching
    let request2 = TestRequest {
        query: "query".to_string(),
        id: 2,
    };
    let error = cached_service
        .call(request2.clone())
        .await
        .expect_err("should error");
    assert_eq!(error.to_string(), "Parse error: test error");

    // Call again to ensure error comes from cache (Arc cloning)
    let cached_error = cached_service
        .call(request2)
        .await
        .expect_err("should error");
    assert_eq!(cached_error.to_string(), "Parse error: test error");

    // Verify service was only called twice (once for each unique key)
    assert_eq!(mock_service.call_count(), 2);
}

#[tokio::test]
async fn test_overloaded_errors_never_cached() {
    // Create a service that returns Overloaded errors
    let mock_service = MockService::new(vec![
        Err(Box::new(Overloaded::new()) as BoxError),
        Err(Box::new(Overloaded::new()) as BoxError),
        Err(Box::new(Overloaded::new()) as BoxError),
    ]);

    // Create cache layer with a predicate that would cache ANY error
    // This tests that Overloaded errors are excluded even when the predicate says to cache
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |_err: &ArcError| true, // Cache ALL errors (but Overloaded should still be excluded)
    );

    let mut cached_service = cache_layer.layer(mock_service.clone());

    let request = TestRequest {
        query: "overloaded query".to_string(),
        id: 1,
    };

    // First call should hit the service and NOT cache the Overloaded error
    let error1 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    
    // Verify it's an Overloaded error
    assert!(error1.is::<Overloaded>());
    assert_eq!(mock_service.call_count(), 1);

    // Second call with same query should hit the service again (not cached)
    let error2 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    
    // Verify it's still an Overloaded error and service was called again
    assert!(error2.is::<Overloaded>());
    assert_eq!(mock_service.call_count(), 2); // Service called again because error wasn't cached

    // Third call should also hit the service (still not cached)
    let error3 = cached_service
        .call(request.clone())
        .await
        .expect_err("should error");
    
    assert!(error3.is::<Overloaded>());
    assert_eq!(mock_service.call_count(), 3); // Service called again because error wasn't cached
}
