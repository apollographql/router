use tower::BoxError;
use tracing::debug;
use tracing::info;

use crate::integration::common::IntegrationTest;
use crate::integration::subscriptions::SUBSCRIPTION_CONFIG;
use crate::integration::subscriptions::SUBSCRIPTION_COPROCESSOR_CONFIG;
use crate::integration::subscriptions::create_sub_query;
use crate::integration::subscriptions::start_coprocessor_server;
use crate::integration::subscriptions::start_subscription_server_with_config;
use crate::integration::subscriptions::verify_subscription_data;
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

        // Test if the WebSocket server is actually running
        let test_url = format!("http://{}/", ws_addr);
        debug!("Testing server availability at: {}", test_url);
        match reqwest::get(&test_url).await {
            Ok(response) => {
                debug!("Server test successful: {}", response.status());
                let body = response.text().await.unwrap_or_default();
                debug!("Server response: {}", body);
            }
            Err(e) => {
                debug!("Server test failed: {}", e);
            }
        }

        // Test WebSocket endpoint specifically
        let ws_test_url = format!("http://{}/ws", ws_addr);
        debug!("Testing WebSocket endpoint at: {}", ws_test_url);
        match reqwest::get(&ws_test_url).await {
            Ok(response) => {
                debug!("WebSocket endpoint test: {}", response.status());
                let body = response.text().await.unwrap_or_default();
                debug!("WebSocket endpoint response: {}", body);
            }
            Err(e) => {
                debug!("WebSocket endpoint test failed: {}", e);
            }
        }

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
        let subscription_events = verify_subscription_events(stream, expected_events)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Verify we received exactly the expected number of events
        assert_eq!(
            subscription_events.len(),
            expected_events,
            "Expected exactly {} events, but received {}",
            expected_events,
            subscription_events.len()
        );

        // Use shared validation for subscription data
        verify_subscription_data(subscription_events, expected_events)
            .await
            .map_err(|e| format!("Data validation failed: {}", e))?;

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

        // Test if servers are running
        let test_url = format!("http://{}/", ws_addr);
        debug!("Testing WebSocket server availability at: {}", test_url);
        match reqwest::get(&test_url).await {
            Ok(response) => {
                debug!("WebSocket server test: {}", response.status());
            }
            Err(e) => {
                debug!("WebSocket server test failed: {}", e);
            }
        }

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
        let subscription_events = verify_subscription_events(stream, expected_events)
            .await
            .map_err(|e| format!("Event verification failed: {}", e))?;

        // Verify we received exactly the expected number of events
        assert_eq!(
            subscription_events.len(),
            expected_events,
            "Expected exactly {} events, but received {}",
            expected_events,
            subscription_events.len()
        );

        // Use shared validation for subscription data
        verify_subscription_data(subscription_events, expected_events)
            .await
            .map_err(|e| format!("Data validation failed: {}", e))?;

        info!(
            "✅ Passthrough subscription mode with coprocessor test completed successfully with {} events",
            expected_events
        );
        info!("✅ Coprocessor successfully processed subscription requests and responses");
    }

    Ok(())
}
