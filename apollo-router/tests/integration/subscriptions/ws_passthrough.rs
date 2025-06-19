use serde_json::Value;
use tower::BoxError;
use tracing::debug;
use tracing::info;

use crate::integration::common::IntegrationTest;
use crate::integration::subscriptions::SUBSCRIPTION_CONFIG;
use crate::integration::subscriptions::SUBSCRIPTION_COPROCESSOR_CONFIG;
use crate::integration::subscriptions::create_sub_query;
use crate::integration::subscriptions::start_coprocessor_server;
use crate::integration::subscriptions::start_subscription_server_with_config;
use crate::integration::subscriptions::start_subscription_server_with_payloads;
use crate::integration::subscriptions::verify_subscription_events;

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Configure exactly 5 events for this test, but allow for timing issues
        let expected_events = 5;
        let interval_ms = 10; // Slower interval to reduce timing issues

        // Start subscription server with specific configuration
        let (ws_addr, http_server) =
            start_subscription_server_with_config(expected_events, interval_ms).await;

        // Configure router to use WebSocket server for accounts subgraph subscriptions
        let ws_url = format!("ws://{}/ws", ws_addr);
        let config = SUBSCRIPTION_CONFIG
            .replace("http://localhost:4005", &http_server.uri())
            .replace("http://localhost:4001", &ws_url)
            .replace("rng:", "accounts:");

        info!("WebSocket server started at: {}", ws_url);
        debug!("Generated router configuration:\n{}", config);
        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        // Use the configured query that matches our server configuration
        let query = create_sub_query(interval_ms, expected_events);
        let (_, response) = router.run_subscription(&query).await;

        // Expect the router to handle the subscription successfully
        assert!(
            response.status().is_success(),
            "Subscription request failed with status: {}",
            response.status()
        );

        let stream = response.bytes_stream();
        let expected_events_data = vec![
            serde_json::json!(null), // Initial event processed first
            serde_json::json!({
                "name": "User 1",
                "reviews": [{"body": "Review 1 from user 1"}]
            }),
            serde_json::json!({
                "name": "User 2",
                "reviews": [{"body": "Review 2 from user 2"}]
            }),
            serde_json::json!({
                "name": "User 3",
                "reviews": [{"body": "Review 3 from user 3"}]
            }),
            serde_json::json!({
                "name": "User 4",
                "reviews": [{"body": "Review 4 from user 4"}]
            }),
        ][..expected_events]
            .to_vec();
        let _subscription_events = verify_subscription_events(stream, expected_events_data)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Check for errors in router logs
        router.assert_no_error_logs();

        info!(
            "✅ Passthrough subscription mode test completed successfully with {} events",
            expected_events
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_with_coprocessor() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Configure exactly 7 events for this test (different from first test)
        let expected_events = 7;
        let interval_ms = 10;

        // Start subscription server and coprocessor
        let (ws_addr, http_server) =
            start_subscription_server_with_config(expected_events, interval_ms).await;
        let coprocessor_server = start_coprocessor_server().await;

        // Configure router to use WebSocket server for accounts subgraph subscriptions
        // and coprocessor for request/response processing
        let ws_url = format!("ws://{}/ws", ws_addr);
        let config = SUBSCRIPTION_COPROCESSOR_CONFIG
            .replace("http://localhost:4005", &http_server.uri())
            .replace("http://localhost:4001", &ws_url)
            .replace("http://localhost:8080", &coprocessor_server.uri())
            .replace("rng:", "accounts:");

        info!("WebSocket server started at: {}", ws_url);
        info!(
            "Coprocessor server started at: {}",
            coprocessor_server.uri()
        );
        debug!("Generated router configuration:\n{}", config);

        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        // Use the configured query that matches our server configuration
        let query = create_sub_query(interval_ms, expected_events);
        let (_, response) = router.run_subscription(&query).await;

        // Expect the router to handle the subscription successfully
        assert!(
            response.status().is_success(),
            "Subscription request failed with status: {}",
            response.status()
        );

        let stream = response.bytes_stream();
        let expected_events_data = vec![
            serde_json::json!(null), // Initial event processed first
            serde_json::json!({
                "name": "User 1",
                "reviews": [{"body": "Review 1 from user 1"}]
            }),
            serde_json::json!({
                "name": "User 2",
                "reviews": [{"body": "Review 2 from user 2"}]
            }),
            serde_json::json!({
                "name": "User 3",
                "reviews": [{"body": "Review 3 from user 3"}]
            }),
            serde_json::json!({
                "name": "User 4",
                "reviews": [{"body": "Review 4 from user 4"}]
            }),
            serde_json::json!({
                "name": "User 5",
                "reviews": [{"body": "Review 5 from user 5"}]
            }),
            serde_json::json!({
                "name": "User 6",
                "reviews": [{"body": "Review 6 from user 6"}]
            }),
        ][..expected_events]
            .to_vec();
        let _subscription_events = verify_subscription_events(stream, expected_events_data)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Check for errors in router logs
        router.assert_no_error_logs();

        info!(
            "✅ Passthrough subscription mode with coprocessor test completed successfully with {} events",
            expected_events
        );
        info!("✅ Coprocessor successfully processed subscription requests and responses");
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_error_payload() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Create custom payloads: one normal event, one error event (no reviews field)
        let custom_payloads = vec![
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 1",
                        "reviews": [{
                            "body": "Review 1 from user 1"
                        }]
                    }
                }
            }),
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 2"
                        // Missing reviews field to test error handling
                    }
                },
                "errors": []
            }),
        ];
        let interval_ms = 10;

        // Start subscription server with custom payloads
        let (ws_addr, http_server) =
            start_subscription_server_with_payloads(custom_payloads.clone(), interval_ms).await;

        // Configure router to use WebSocket server for accounts subgraph subscriptions
        let ws_url = format!("ws://{}/ws", ws_addr);
        let config = SUBSCRIPTION_CONFIG
            .replace("http://localhost:4005", &http_server.uri())
            .replace("http://localhost:4001", &ws_url)
            .replace("rng:", "accounts:");

        info!("WebSocket server started at: {}", ws_url);

        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        let subscription_query = create_sub_query(interval_ms, custom_payloads.len());

        let response = router
            .execute_query(
                crate::integration::common::Query::builder()
                    .body(serde_json::json!({
                        "query": subscription_query
                    }))
                    .headers(std::collections::HashMap::from([(
                        "Accept".to_string(),
                        "multipart/mixed;subscriptionSpec=1.0".to_string(),
                    )]))
                    .build(),
            )
            .await;

        assert!(
            response.1.status().is_success(),
            "Subscription request failed with status: {}",
            response.1.status()
        );

        let stream = response.1.bytes_stream();
        let expected_events_data = vec![
            serde_json::json!(null), // Initial event processed first
            serde_json::json!({
                "name": "User 1",
                "reviews": [{"body": "Review 1 from user 1"}]
            }),
        ];
        let _subscription_events = verify_subscription_events(stream, expected_events_data)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Check for errors in router logs
        router.assert_no_error_logs();

        info!(
            "✅ WebSocket passthrough with error payload test completed successfully with {} events",
            custom_payloads.len()
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_pure_error_payload() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Create custom payloads: one normal event, one pure error event (no data, only errors)
        let custom_payloads = vec![
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 1",
                        "reviews": [{
                            "body": "Review 1 from user 1"
                        }]
                    }
                }
            }),
            serde_json::json!({
                // No data attribute at all
            }),
        ];
        let interval_ms = 10;

        // Start subscription server with custom payloads
        let (ws_addr, http_server) =
            start_subscription_server_with_payloads(custom_payloads.clone(), interval_ms).await;

        // Configure router to use WebSocket server for accounts subgraph subscriptions
        let ws_url = format!("ws://{}/ws", ws_addr);
        let config = SUBSCRIPTION_CONFIG
            .replace("http://localhost:4005", &http_server.uri())
            .replace("http://localhost:4001", &ws_url)
            .replace("rng:", "accounts:");

        info!("WebSocket server started at: {}", ws_url);

        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        let subscription_query = create_sub_query(interval_ms, custom_payloads.len());

        let response = router
            .execute_query(
                crate::integration::common::Query::builder()
                    .body(serde_json::json!({
                        "query": subscription_query
                    }))
                    .headers(std::collections::HashMap::from([(
                        "Accept".to_string(),
                        "multipart/mixed;subscriptionSpec=1.0".to_string(),
                    )]))
                    .build(),
            )
            .await;

        assert!(
            response.1.status().is_success(),
            "Subscription request failed with status: {}",
            response.1.status()
        );

        let stream = response.1.bytes_stream();
        // Pure error test: events received in order they are processed
        let expected_events_data = vec![
            serde_json::json!(null), // First event processed: has no data
            serde_json::json!({
                "name": "User 1",
                "reviews": [{"body": "Review 1 from user 1"}]
            }),
        ];
        let _subscription_events = verify_subscription_events(stream, expected_events_data)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Check for errors in router logs
        router.assert_no_error_logs();

        info!(
            "✅ WebSocket passthrough with pure error payload test completed successfully with {} events",
            custom_payloads.len()
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[should_panic]
async fn test_subscription_ws_passthrough_pure_error_payload_with_coprocessor() {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Create custom payloads: one normal event, one pure error event (no data, only errors)
        let custom_payloads = vec![
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 1",
                        "reviews": [{
                            "body": "Review 1 from user 1"
                        }]
                    }
                }
            }),
            serde_json::json!({
                // Missing required "data" or "errors" field - this should cause coprocessor to fail
            }),
            // This event will never be received
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 2",
                        "reviews": [{
                            "body": "Review 1 from user 1"
                        }]
                    }
                }
            }),
        ];
        let interval_ms = 10;

        // Start subscription server and coprocessor
        let (ws_addr, http_server) =
            start_subscription_server_with_payloads(custom_payloads.clone(), interval_ms).await;
        let coprocessor_server = start_coprocessor_server().await;

        // Configure router to use WebSocket server for accounts subgraph subscriptions
        // and coprocessor for request/response processing
        let ws_url = format!("ws://{}/ws", ws_addr);
        let config = SUBSCRIPTION_COPROCESSOR_CONFIG
            .replace("http://localhost:4005", &http_server.uri())
            .replace("http://localhost:4001", &ws_url)
            .replace("http://localhost:8080", &coprocessor_server.uri())
            .replace("rng:", "accounts:");

        info!("WebSocket server started at: {}", ws_url);
        info!(
            "Coprocessor server started at: {}",
            coprocessor_server.uri()
        );

        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        let subscription_query = create_sub_query(interval_ms, custom_payloads.len());

        let response = router
            .execute_query(
                crate::integration::common::Query::builder()
                    .body(serde_json::json!({
                        "query": subscription_query
                    }))
                    .headers(std::collections::HashMap::from([(
                        "Accept".to_string(),
                        "multipart/mixed;subscriptionSpec=1.0".to_string(),
                    )]))
                    .build(),
            )
            .await;

        assert!(
            response.1.status().is_success(),
            "Subscription request failed with status: {}",
            response.1.status()
        );

        let stream = response.1.bytes_stream();

        // Now we're storing raw responses, so expect the actual GraphQL response structure
        let expected_events_data = vec![
            serde_json::json!({
                "data": {
                    "userWasCreated": {
                        "name": "User 1",
                        "reviews": [{"body": "Review 1 from user 1"}]
                    }
                }
            }),
            Value::Null, // The empty object {} should cause some kind of error response
                         // We'll let the test fail to see what we actually get
        ];
        let _subscription_events = verify_subscription_events(stream, expected_events_data)
            .await
            .map_err(|e| format!("Event verification failed: {}", e));

        // Check for errors in router logs this should fail!
        router.assert_no_error_logs();
    }
}
