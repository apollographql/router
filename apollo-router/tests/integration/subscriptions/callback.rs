use tower::BoxError;

use crate::integration::common::IntegrationTest;
use crate::integration::common::graph_os_enabled;
use crate::integration::subscriptions::CALLBACK_CONFIG;
use crate::integration::subscriptions::CallbackTestState;
use crate::integration::subscriptions::start_callback_server;
use crate::integration::subscriptions::start_callback_subgraph_server;
use crate::integration::subscriptions::start_callback_subgraph_server_with_payloads;

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    let nb_events = 3;
    let interval_ms = 100;

    // Start callback server to receive router callbacks
    let (callback_addr, callback_state) = start_callback_server().await;
    let callback_url = format!("http://{callback_addr}/callback");

    // Start mock subgraph server that will send callbacks
    let subgraph_server =
        start_callback_subgraph_server(nb_events, interval_ms, callback_url.clone()).await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .build()
        .await;

    // Reserve ports using the existing external ports and allocate new ones
    let callback_receiver_port = callback_addr.port();
    let _callback_listener_port = router.reserve_address("CALLBACK_LISTENER_PORT");
    router.set_address("CALLBACK_RECEIVER_PORT", callback_receiver_port);
    router.set_address_from_uri("SUBGRAPH_PORT", &subgraph_server.uri());

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

    // Wait until the router has dispatched all `nb_events` "next"
    // callbacks plus the trailing "complete" callback. Previously this
    // was a fixed-formula sleep `(nb_events * interval_ms) + 1000`,
    // which mixed event-spacing math with a buffer for delivery
    // latency — under CI load the +1000ms buffer was not always
    // enough. Poll the callback receiver until the expected count
    // arrives, with a generous deadline.
    wait_for_callbacks(
        &callback_state,
        nb_events + 1,
        tokio::time::Duration::from_secs(30),
    )
    .await;

    // Verify callbacks were received - expect default user events
    let expected_user_events = vec![
        serde_json::json!({
            "name": "User 1",
            "reviews": [{
                "body": "Review 1 from user 1"
            }]
        }),
        serde_json::json!({
            "name": "User 2",
            "reviews": [{
                "body": "Review 2 from user 2"
            }]
        }),
        serde_json::json!({
            "name": "User 3",
            "reviews": [{
                "body": "Review 3 from user 3"
            }]
        }),
    ];
    verify_callback_events(&callback_state, expected_user_events).await?;

    // Check for errors in router logs
    router.assert_no_error_logs();

    Ok(())
}

/// Poll the callback receiver until the router has delivered
/// `expected_count` callbacks (counting both `next` and `complete`),
/// with a hard `deadline`. Replaces the prior formula-based sleep.
async fn wait_for_callbacks(
    callback_state: &CallbackTestState,
    expected_count: usize,
    deadline: tokio::time::Duration,
) {
    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        let count = callback_state.received_callbacks.lock().len();
        if count >= expected_count {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    let count = callback_state.received_callbacks.lock().len();
    panic!("expected {expected_count} callbacks within {deadline:?}, got {count}");
}

/// Poll the router's HTTP endpoint until it accepts a request
/// (any HTTP-level response is sufficient), or `deadline` expires.
///
/// `router.assert_started().await` only waits for the
/// `GraphQL endpoint exposed at ...` log line emitted in
/// `axum_factory::axum_http_server_factory::create` immediately
/// after the `TcpListener` is bound but BEFORE the spawned
/// server task is actually polled. The kernel will accept TCP
/// handshakes against the bound listener as soon as `bind()`
/// returns, but the userspace `accept()` loop is not running
/// yet. Under CI scheduling pressure (e.g. flake-bash with 10x
/// parallel branches) the gap between log-emit and the accept
/// loop being polled can be long enough that an early POST
/// from the test client is closed by the kernel's
/// RST-on-overflow path, producing
/// `reqwest::Error: error sending request for url (...)` —
/// the surface that crashed
/// `test_subscription_callback_error_payload` on
/// `test-amd_linux_test`.
///
/// A HEAD against `/` won't return a useful status (the
/// supergraph rejects non-POST GraphQL), but it WILL complete
/// the request once the server task is actually polling
/// connections, which is the signal we need.
async fn wait_for_router_ready(url: &str, deadline: tokio::time::Duration) {
    let start = tokio::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(tokio::time::Duration::from_secs(1))
        .build()
        .expect("build reqwest client");
    while start.elapsed() < deadline {
        if client.head(url).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    panic!("router at {url} did not accept HTTP requests within {deadline:?}");
}

async fn verify_callback_events(
    callback_state: &CallbackTestState,
    expected_user_events: Vec<serde_json::Value>,
) -> Result<(), BoxError> {
    use pretty_assertions::assert_eq;

    let callbacks = callback_state.received_callbacks.lock().clone();

    // Should have received: expected_user_events.len() "next" callbacks + 1 "complete" callback
    let next_callbacks: Vec<_> = callbacks.iter().filter(|c| c.action == "next").collect();
    let complete_callbacks: Vec<_> = callbacks
        .iter()
        .filter(|c| c.action == "complete")
        .collect();

    // Note: We don't check next_callbacks.len() == expected_user_events.len()
    // because some callbacks may not have userWasCreated data (e.g., pure error payloads)

    assert_eq!(
        complete_callbacks.len(),
        1,
        "Expected 1 'complete' callback, got {}. All callbacks: {:?}",
        complete_callbacks.len(),
        callbacks
    );

    // Extract userWasCreated events for validation
    let mut actual_user_events = Vec::new();
    for callback in &next_callbacks {
        if let Some(payload) = &callback.payload
            && let Some(data) = payload.get("data")
            && let Some(user_created) = data.get("userWasCreated")
        {
            actual_user_events.push(user_created.clone());
        }
        // If there's a data field but no userWasCreated, it's an empty/error case
        // If there's no data field (pure error payload), we don't extract anything
    }

    // Simple equality comparison using pretty_assertions
    assert_eq!(
        actual_user_events, expected_user_events,
        "Callback user events do not match expected events"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback_error_scenarios() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    // Test 1: Invalid callback payload (missing fields)
    let (callback_addr, callback_state) = start_callback_server().await;

    let client = reqwest::Client::new();
    let callback_url = format!("http://{callback_addr}/callback/test-id");

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
        let mut ids = callback_state.subscription_ids.lock();
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

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback_error_payload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    let interval_ms = 100;

    // Create custom payloads: one normal event, one error event (no body, empty errors)
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

    // Start callback server to receive router callbacks
    let (callback_addr, callback_state) = start_callback_server().await;
    let callback_url = format!("http://{callback_addr}/callback");

    // Start mock subgraph server with custom payloads
    let subgraph_server = start_callback_subgraph_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        callback_url.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .build()
        .await;

    // Reserve ports using the existing external ports and allocate new ones
    let callback_receiver_port = callback_addr.port();
    let _callback_listener_port = router.reserve_address("CALLBACK_LISTENER_PORT");
    router.set_address("CALLBACK_RECEIVER_PORT", callback_receiver_port);
    router.set_address_from_uri("SUBGRAPH_PORT", &subgraph_server.uri());

    router.start().await;
    router.assert_started().await;

    // `assert_started` only matches the `GraphQL endpoint exposed`
    // log line, which is emitted before the axum server task is
    // actually polling connections — see `wait_for_router_ready`
    // doc for details. Poll the bound address until the router
    // actually answers an HTTP request before sending the
    // subscription POST, so the test's initial `execute_query`
    // can't race the kernel's RST-on-overflow path under heavy CI
    // scheduling pressure (the failure surface previously observed
    // on `test-amd_linux_test`).
    let router_url = format!("http://{}/", router.bind_address());
    wait_for_router_ready(&router_url, tokio::time::Duration::from_secs(60)).await;

    let subscription_query = r#"subscription { userWasCreated(intervalMs: 100, nbEvents: 2) { name reviews { body } } }"#;

    // Send subscription request to router
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

    // Poll callbacks instead of formula sleep — see
    // `wait_for_callbacks` doc above. `+1` accounts for the trailing
    // `complete` callback.
    wait_for_callbacks(
        &callback_state,
        custom_payloads.len() + 1,
        tokio::time::Duration::from_secs(30),
    )
    .await;

    // Verify callbacks were received - expect the exact user events from custom payloads
    let expected_user_events = vec![
        serde_json::json!({
            "name": "User 1",
            "reviews": [{
                "body": "Review 1 from user 1"
            }]
        }),
        serde_json::json!({
            "name": "User 2"
            // Missing reviews field to test error handling
        }),
    ];
    verify_callback_events(&callback_state, expected_user_events).await?;

    // Check for errors in router logs
    router.assert_no_error_logs();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_callback_pure_error_payload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    let interval_ms = 100;

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
            "errors": []
            // No data attribute at all
        }),
    ];

    // Start callback server to receive router callbacks
    let (callback_addr, callback_state) = start_callback_server().await;
    let callback_url = format!("http://{callback_addr}/callback");

    // Start mock subgraph server with custom payloads
    let subgraph_server = start_callback_subgraph_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        callback_url.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .build()
        .await;

    // Reserve ports using the existing external ports and allocate new ones
    let callback_receiver_port = callback_addr.port();
    let _callback_listener_port = router.reserve_address("CALLBACK_LISTENER_PORT");
    router.set_address("CALLBACK_RECEIVER_PORT", callback_receiver_port);
    router.set_address_from_uri("SUBGRAPH_PORT", &subgraph_server.uri());

    router.start().await;
    router.assert_started().await;

    // See `wait_for_router_ready` doc — `assert_started` only matches
    // the `GraphQL endpoint exposed` log line, emitted before the
    // axum server task is actually polling connections. Without this
    // probe, the first `execute_query` POST can race the kernel's
    // RST-on-overflow path under heavy CI scheduling pressure (the
    // surface that crashed this test on CircleCI build 378842,
    // `test-amd_linux_test`).
    let router_url = format!("http://{}/", router.bind_address());
    wait_for_router_ready(&router_url, tokio::time::Duration::from_secs(60)).await;

    let subscription_query = r#"subscription { userWasCreated(intervalMs: 100, nbEvents: 2) { name reviews { body } } }"#;

    // Send subscription request to router
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

    // Poll callbacks instead of formula sleep — see
    // `wait_for_callbacks` doc above. `+1` accounts for the trailing
    // `complete` callback.
    wait_for_callbacks(
        &callback_state,
        custom_payloads.len() + 1,
        tokio::time::Duration::from_secs(30),
    )
    .await;

    // Verify callbacks were received - expect only 1 user event since second callback has no userWasCreated data
    let expected_user_events = vec![
        serde_json::json!({
            "name": "User 1",
            "reviews": [{
                "body": "Review 1 from user 1"
            }]
        }),
        // Second callback has no userWasCreated data (pure error payload), so nothing is extracted from it
    ];
    verify_callback_events(&callback_state, expected_user_events).await?;

    // Check for errors in router logs
    router.assert_no_error_logs();

    Ok(())
}
