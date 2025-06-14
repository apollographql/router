use std::time::Duration;

use tower::BoxError;
use tower::load_shed::error::Overloaded;

use super::*;
use crate::test_utils::tower_test::TowerTest;

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

#[tokio::test]
async fn test_cache_successful_responses() -> Result<(), BoxError> {
    // Create cache layer with key extraction based on query string
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |_err: &ArcError| false, // Don't cache any errors for this test
    );

    let request = TestRequest {
        query: "query { user }".to_string(),
        id: 1,
    };

    // First call should hit the service
    let response1 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(1);
            let (received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            // Verify request passthrough
            assert_eq!(received_req.query, "query { user }");
            assert_eq!(received_req.id, 1);

            // Send success response
            send_response.send_response(TestResponse {
                data: "success".to_string(),
            });
        })
        .await?;

    assert_eq!(response1.data, "success");

    // Second call with same query should hit cache (no downstream call)
    let response2 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await?;

    assert_eq!(response2.data, "success"); // Should be the same cached response

    // Different query should hit service
    let different_request = TestRequest {
        query: "query { posts }".to_string(),
        id: 2,
    };

    let response3 = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(different_request, |mut downstream| async move {
            downstream.allow(1);
            let (received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            assert_eq!(received_req.query, "query { posts }");

            send_response.send_response(TestResponse {
                data: "second_call_response".to_string(),
            });
        })
        .await?;

    assert_eq!(response3.data, "second_call_response");
    Ok(())
}

#[tokio::test]
async fn test_cache_specific_error_types() -> Result<(), BoxError> {
    // Create cache layer that caches ParseError but not other errors
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    let request = TestRequest {
        query: "invalid query".to_string(),
        id: 1,
    };

    // First call should hit the service and cache the error
    let error1 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(1);
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            // Send ParseError
            send_response.send_error(ParseError {
                message: "Invalid syntax".to_string(),
            });
        })
        .await
        .expect_err("should error");

    assert_eq!(error1.to_string(), "Parse error: Invalid syntax");

    // Second call with same query should hit cache and return cached error (no downstream call)
    let error2 = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(request, |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await
        .expect_err("should error");

    assert_eq!(error2.to_string(), "Parse error: Invalid syntax");
    Ok(())
}

#[tokio::test]
async fn test_custom_key_extraction() -> Result<(), BoxError> {
    // Create cache layer with key extraction based on ID instead of query
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.id.to_string(),
        |_err: &ArcError| false, // Don't cache errors
    );

    let request1 = TestRequest {
        query: "query { user }".to_string(),
        id: 1,
    };

    let request2 = TestRequest {
        query: "query { posts }".to_string(), // Different query
        id: 1,                                // Same ID
    };

    // First call
    let response1 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request1, |mut downstream| async move {
            downstream.allow(1);
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            send_response.send_response(TestResponse {
                data: "response1".to_string(),
            });
        })
        .await?;

    assert_eq!(response1.data, "response1");

    // Second call with different query but same ID should hit cache
    let response2 = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(request2, |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await?;

    assert_eq!(response2.data, "response1"); // Same cached response
    Ok(())
}

#[tokio::test]
async fn test_cache_capacity_limit() -> Result<(), BoxError> {
    // Create cache layer with capacity of 2
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        2,
        |req: &TestRequest| req.id.to_string(),
        |_err: &ArcError| false, // Don't cache errors
    );

    // Make 3 requests with different IDs - all should hit the service initially
    for i in 1..=3 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };

        let response_data = format!("response{}", i);
        let response = TowerTest::builder()
            .layer(cache_layer.clone())
            .oneshot(request, move |mut downstream| async move {
                downstream.allow(1);
                let (_received_req, send_response) = downstream
                    .next_request()
                    .await
                    .expect("should receive downstream request");

                send_response.send_response(TestResponse {
                    data: response_data,
                });
            })
            .await?;

        assert_eq!(response.data, format!("response{}", i));
    }

    // Make more requests than cache capacity to force evictions
    for i in 4..=5 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };

        let response_data = format!("response{}", i);
        let response = TowerTest::builder()
            .layer(cache_layer.clone())
            .oneshot(request, move |mut downstream| async move {
                downstream.allow(1);
                let (_received_req, send_response) = downstream
                    .next_request()
                    .await
                    .expect("should receive downstream request");

                send_response.send_response(TestResponse {
                    data: response_data,
                });
            })
            .await?;

        assert_eq!(response.data, format!("response{}", i));
    }

    // Test cache behavior after evictions - some entries should be cached, others should hit service
    // We can't predict exactly which ones are evicted, but we can test that the cache works
    let mut cache_hits = 0;
    let mut service_calls = 0;

    for i in 1..=5 {
        let request = TestRequest {
            query: "query".to_string(),
            id: i,
        };

        // Try with no downstream expectation first to see if it's cached
        let result = TowerTest::builder()
            .layer(cache_layer.clone())
            .timeout(Duration::from_millis(100))
            .oneshot(request.clone(), |mut downstream| async move {
                downstream.allow(0); // No downstream calls expected if cached
            })
            .await;

        if result.is_ok() {
            cache_hits += 1;
        } else {
            // Not cached, needs service call
            service_calls += 1;
            let response_data = format!("response{}", i);
            let _response = TowerTest::builder()
                .layer(cache_layer.clone())
                .oneshot(request, move |mut downstream| async move {
                    downstream.allow(1);
                    let (_received_req, send_response) = downstream
                        .next_request()
                        .await
                        .expect("should receive downstream request");

                    send_response.send_response(TestResponse {
                        data: response_data,
                    });
                })
                .await?;
        }
    }

    // Due to cache capacity of 2, we should have some cache hits and some service calls
    assert!(
        cache_hits >= 1,
        "Expected at least 1 cache hit, got {}",
        cache_hits
    );
    assert!(
        service_calls >= 1,
        "Expected at least 1 service call, got {}",
        service_calls
    );
    assert_eq!(cache_hits + service_calls, 5, "Total should be 5");

    Ok(())
}

#[tokio::test]
async fn test_query_parse_cache_convenience_function() -> Result<(), BoxError> {
    // Test the convenience function for query parsing
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = query_parse_cache(
        10,
        |req: &TestRequest| req.query.clone(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    let request = TestRequest {
        query: "invalid query".to_string(),
        id: 1,
    };

    // First call should cache the parse error
    let _error1 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(1);
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            send_response.send_error(ParseError {
                message: "syntax error".to_string(),
            });
        })
        .await
        .expect_err("should error");

    // Second call should return cached error (no downstream call)
    let _error2 = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(request, |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await
        .expect_err("should error");

    Ok(())
}

#[tokio::test]
async fn test_response_and_error_conversion_to_arc() -> Result<(), BoxError> {
    // Test that responses and errors are properly converted to Arc internally
    let original_response = TestResponse {
        data: "test".to_string(),
    };
    let original_error = ParseError {
        message: "test error".to_string(),
    };

    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.id.to_string(),
        |err: &ArcError| {
            // Cache ParseError but not other error types
            err.is::<ParseError>()
        },
    );

    // Test successful response caching
    let request1 = TestRequest {
        query: "query".to_string(),
        id: 1,
    };

    let response = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request1.clone(), {
            let response_clone = original_response.clone();
            move |mut downstream| async move {
                downstream.allow(1);
                let (_received_req, send_response) = downstream
                    .next_request()
                    .await
                    .expect("should receive downstream request");

                send_response.send_response(response_clone);
            }
        })
        .await?;

    assert_eq!(*response, original_response);

    // Call again to ensure it comes from cache (Arc cloning)
    let cached_response = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request1, |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await?;

    assert_eq!(*cached_response, original_response);

    // Test error caching
    let request2 = TestRequest {
        query: "query".to_string(),
        id: 2,
    };

    let error = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request2.clone(), {
            let error_clone = original_error.clone();
            move |mut downstream| async move {
                downstream.allow(1);
                let (_received_req, send_response) = downstream
                    .next_request()
                    .await
                    .expect("should receive downstream request");

                send_response.send_error(error_clone);
            }
        })
        .await
        .expect_err("should error");

    assert_eq!(error.to_string(), "Parse error: test error");

    // Call again to ensure error comes from cache (Arc cloning)
    let cached_error = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(request2, |mut downstream| async move {
            downstream.allow(0); // No downstream calls expected (cache hit)
        })
        .await
        .expect_err("should error");

    assert_eq!(cached_error.to_string(), "Parse error: test error");
    Ok(())
}

#[tokio::test]
async fn test_overloaded_errors_never_cached() -> Result<(), BoxError> {
    // Create cache layer with a predicate that would cache ANY error
    // This tests that Overloaded errors are excluded even when the predicate says to cache
    let cache_layer: CacheLayer<TestRequest, TestResponse, String, _, _> = CacheLayer::new(
        10,
        |req: &TestRequest| req.query.clone(),
        |_err: &ArcError| true, // Cache ALL errors (but Overloaded should still be excluded)
    );

    let request = TestRequest {
        query: "overloaded query".to_string(),
        id: 1,
    };

    // First call should hit the service and NOT cache the Overloaded error
    let error1 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(1);
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            send_response.send_error(Overloaded::new());
        })
        .await
        .expect_err("should error");

    // Verify it's an Overloaded error (check by error message since TowerTest wrapping affects type checks)
    assert_eq!(error1.to_string(), "service overloaded");

    // Second call with same query should hit the service again (not cached)
    let error2 = TowerTest::builder()
        .layer(cache_layer.clone())
        .oneshot(request.clone(), |mut downstream| async move {
            downstream.allow(1); // Service should be called again because error wasn't cached
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            send_response.send_error(Overloaded::new());
        })
        .await
        .expect_err("should error");

    // Verify it's still an Overloaded error
    assert_eq!(error2.to_string(), "service overloaded");

    // Third call should also hit the service (still not cached)
    let error3 = TowerTest::builder()
        .layer(cache_layer)
        .oneshot(request, |mut downstream| async move {
            downstream.allow(1); // Service should be called again because error wasn't cached
            let (_received_req, send_response) = downstream
                .next_request()
                .await
                .expect("should receive downstream request");

            send_response.send_error(Overloaded::new());
        })
        .await
        .expect_err("should error");

    assert_eq!(error3.to_string(), "service overloaded");
    Ok(())
}
