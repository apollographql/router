use tower::BoxError;

use crate::integration::common::IntegrationTest;
use crate::integration::subscriptions::CALLBACK_CONFIG;
use crate::integration::subscriptions::CallbackTestState;
use crate::integration::subscriptions::start_callback_server;
use crate::integration::subscriptions::start_callback_subgraph_server;
use crate::integration::subscriptions::verify_subscription_data;

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        let nb_events = 3;
        let interval_ms = 100;

        // Start callback server to receive router callbacks
        let (callback_addr, callback_state) = start_callback_server().await;
        let callback_url = format!("http://{}/callback", callback_addr);

        // Start mock subgraph server that will send callbacks
        let subgraph_server =
            start_callback_subgraph_server(nb_events, interval_ms, callback_url.clone()).await;

        // Create router config with callback URL pointing to our callback server
        let config = CALLBACK_CONFIG
            .replace("http://localhost:4002/callback", &callback_url)
            .replace("http://localhost:4001", &subgraph_server.uri());

        let mut router = IntegrationTest::builder()
            .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
            .config(&config)
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        let subscription_query = r#"subscription { userWasCreated(intervalMs: 100, nbEvents: 3) { name reviews { body } } }"#;

        // Send subscription request to router
        // For callback mode, we still need the subscription Accept header to indicate subscription support
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "Accept".to_string(),
            "multipart/mixed;subscriptionSpec=1.0".to_string(),
        );

        let query = crate::integration::common::Query::builder()
            .body(serde_json::json!({
                "query": subscription_query
            }))
            .headers(headers)
            .build();

        let (_trace_id, response) = router.execute_query(query).await;

        // Router should respond with subscription acknowledgment
        assert!(
            response.status().is_success(),
            "Subscription request failed: {}",
            response.status()
        );

        // Wait for callbacks to be sent
        tokio::time::sleep(tokio::time::Duration::from_millis(
            (nb_events as u64 * interval_ms) + 1000,
        ))
        .await;

        // Verify callbacks were received
        verify_callback_events(&callback_state, nb_events).await?;

        tracing::info!("✅ Callback mode subscription test completed successfully");
    } else {
        tracing::warn!(
            "⚠️  Skipping callback test - requires TEST_APOLLO_KEY and TEST_APOLLO_GRAPH_REF"
        );
    }

    Ok(())
}

async fn verify_callback_events(
    callback_state: &CallbackTestState,
    expected_events: usize,
) -> Result<(), BoxError> {
    let callbacks = callback_state.received_callbacks.lock().unwrap().clone();

    // Should have received: nb_events "next" callbacks + 1 "complete" callback
    let next_callbacks: Vec<_> = callbacks.iter().filter(|c| c.action == "next").collect();
    let complete_callbacks: Vec<_> = callbacks
        .iter()
        .filter(|c| c.action == "complete")
        .collect();

    if next_callbacks.len() != expected_events {
        return Err(format!(
            "Expected {} 'next' callbacks, got {}. All callbacks: {:?}",
            expected_events,
            next_callbacks.len(),
            callbacks
        )
        .into());
    }

    if complete_callbacks.len() != 1 {
        return Err(format!(
            "Expected 1 'complete' callback, got {}. All callbacks: {:?}",
            complete_callbacks.len(),
            callbacks
        )
        .into());
    }

    // Verify callback structure
    for (i, callback) in next_callbacks.iter().enumerate() {
        if callback.kind != "subscription" {
            return Err(format!(
                "Callback {} kind should be 'subscription', got: {}",
                i + 1,
                callback.kind
            )
            .into());
        }

        if callback.verifier.is_empty() {
            return Err(format!("Callback {} should have verifier", i + 1).into());
        }

        if callback.payload.is_none() {
            return Err(format!("Callback {} missing payload", i + 1).into());
        }
    }

    // Verify completion callback
    let complete_callback = &complete_callbacks[0];
    if complete_callback.kind != "subscription" {
        return Err("Complete callback kind should be 'subscription'".into());
    }

    if complete_callback.verifier.is_empty() {
        return Err("Complete callback should have verifier".into());
    }

    // Extract userWasCreated events for data validation
    let mut user_events = Vec::new();
    for callback in &next_callbacks {
        if let Some(payload) = &callback.payload {
            if let Some(data) = payload.get("data") {
                if let Some(user_created) = data.get("userWasCreated") {
                    user_events.push(user_created.clone());
                } else {
                    return Err("Callback missing 'userWasCreated' field".into());
                }
            } else {
                return Err("Callback missing 'data' field".into());
            }
        }
    }

    // Use shared validation for subscription data
    verify_subscription_data(user_events, expected_events)
        .await
        .map_err(|e| -> BoxError { e.into() })?;

    tracing::info!(
        "✅ Verified {} callback events with proper structure",
        callbacks.len()
    );
    tracing::info!("✅ All events contain required fields: kind, action, id, verifier");
    tracing::info!("✅ Received proper completion callback");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback_error_scenarios() -> Result<(), BoxError> {
    if std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok() {
        // Test 1: Invalid callback payload (missing fields)
        let (callback_addr, callback_state) = start_callback_server().await;

        let client = reqwest::Client::new();
        let callback_url = format!("http://{}/callback/test-id", callback_addr);

        // Test invalid payload - missing required fields
        let invalid_payload = serde_json::json!({
            "kind": "subscription",
            "action": "next"
            // Missing: id, verifier
        });

        let response = client
            .post(&callback_url)
            .json(&invalid_payload)
            .send()
            .await?;

        // Should return 422 Unprocessable Entity for malformed JSON payload (missing required fields)
        assert_eq!(response.status(), 422, "Invalid payload should return 422");

        // Test 2: ID mismatch between URL and payload
        let mismatched_payload = serde_json::json!({
            "kind": "subscription",
            "action": "next",
            "id": "different-id",
            "verifier": "test-verifier"
        });

        let response = client
            .post(&callback_url)
            .json(&mismatched_payload)
            .send()
            .await?;

        assert_eq!(response.status(), 400, "ID mismatch should return 400");

        // Test 3: Subscription not found (404 scenarios)
        let valid_payload = serde_json::json!({
            "kind": "subscription",
            "action": "check",
            "id": "test-id",
            "verifier": "test-verifier"
        });

        let response = client
            .post(&callback_url)
            .json(&valid_payload)
            .send()
            .await?;

        assert_eq!(
            response.status(),
            404,
            "Unknown subscription should return 404"
        );

        // Test 4: Add subscription ID and test success scenarios
        {
            let mut ids = callback_state.subscription_ids.lock().unwrap();
            ids.push("test-id".to_string());
        }

        // Now check should succeed
        let response = client
            .post(&callback_url)
            .json(&valid_payload)
            .send()
            .await?;

        assert_eq!(response.status(), 204, "Valid check should return 204");

        // Test 5: Test heartbeat with mixed valid/invalid IDs
        let heartbeat_payload = serde_json::json!({
            "kind": "subscription",
            "action": "heartbeat",
            "id": "test-id",
            "ids": ["test-id", "invalid-id"],
            "verifier": "test-verifier"
        });

        let response = client
            .post(&callback_url)
            .json(&heartbeat_payload)
            .send()
            .await?;

        assert_eq!(
            response.status(),
            404,
            "Heartbeat with invalid IDs should return 404"
        );

        // Test 6: Test heartbeat with all valid IDs
        let valid_heartbeat_payload = serde_json::json!({
            "kind": "subscription",
            "action": "heartbeat",
            "id": "test-id",
            "ids": ["test-id"],
            "verifier": "test-verifier"
        });

        let response = client
            .post(&callback_url)
            .json(&valid_heartbeat_payload)
            .send()
            .await?;

        assert_eq!(response.status(), 204, "Valid heartbeat should return 204");

        // Test 7: Test completion callback
        let complete_payload = serde_json::json!({
            "kind": "subscription",
            "action": "complete",
            "id": "test-id",
            "verifier": "test-verifier"
        });

        let response = client
            .post(&callback_url)
            .json(&complete_payload)
            .send()
            .await?;

        assert_eq!(response.status(), 202, "Valid completion should return 202");

        tracing::info!("✅ All callback error scenarios tested successfully");
    } else {
        tracing::warn!(
            "⚠️  Skipping callback error test - requires TEST_APOLLO_KEY and TEST_APOLLO_GRAPH_REF"
        );
    }

    Ok(())
}
