use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

use regex::Regex;
use tower::BoxError;
use tracing::info;

use crate::integration::common::IntegrationTest;
use crate::integration::common::graph_os_enabled;
use crate::integration::subscriptions::SUBSCRIPTION_CONFIG_GRAPHQL_WS;
use crate::integration::subscriptions::SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS;
use crate::integration::subscriptions::SUBSCRIPTION_COPROCESSOR_CONFIG;
use crate::integration::subscriptions::create_sub_query;
use crate::integration::subscriptions::start_coprocessor_server;
use crate::integration::subscriptions::start_subscription_server_with_payloads;
use crate::integration::subscriptions::verify_subscription_events;

/// Poll `/metrics` at ~25ms cadence until `predicate(&body)` returns true or
/// `deadline` elapses. Returns the body that satisfied the predicate.
///
/// On expiry, panics with the last-seen body and elapsed time. This is the
/// uniform fix for client-side observability races against out-of-process
/// router events: the router is a child process, so cross-process `Notify`
/// doesn't apply, and we must deadline-bound an externally observable
/// predicate (contract C6).
///
/// Module-private by design. If a second consumer appears, lift to
/// `tests/common.rs` in a follow-up. Premature lifting is what the
/// project's anti-fan-out rule prevents.
async fn poll_metrics_until<F>(
    router: &IntegrationTest,
    deadline: Duration,
    predicate: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    let start = Instant::now();
    let mut last_body = String::new();
    while start.elapsed() < deadline {
        match router.get_metrics_response().await {
            Ok(resp) => match resp.text().await {
                Ok(body) => {
                    if predicate(&body) {
                        return body;
                    }
                    last_body = body;
                }
                Err(e) => {
                    last_body = format!("<failed to read body: {e}>");
                }
            },
            Err(e) => {
                last_body = format!("<failed to fetch /metrics: {e}>");
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "poll_metrics_until: predicate not satisfied within {:?} (elapsed {:?}); last body:\n{}",
        deadline,
        start.elapsed(),
        last_body,
    );
}

/// Deadline-poll an in-process `AtomicBool` that is set by the mock WS
/// server's close handler in `tests/integration/subscriptions/mod.rs`.
///
/// `is_closed` is set by a separate in-process task (the server-side close
/// handler), so a one-shot `assert!` immediately after the client stream
/// terminates races with the handler. Cadence 25ms, default deadline 5s.
/// On expiry, panics with `test_name` for diagnostic context (per C6).
async fn assert_is_closed_within(
    is_closed: &Arc<AtomicBool>,
    deadline: Duration,
    test_name: &'static str,
) {
    let start = Instant::now();
    while start.elapsed() < deadline
        && !is_closed.load(std::sync::atomic::Ordering::Relaxed)
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        is_closed.load(std::sync::atomic::Ordering::Relaxed),
        "is_closed not set within {deadline:?} in {test_name} (elapsed {:?})",
        start.elapsed(),
    );
}

/// Creates an expected subscription event payload for a schema reload
fn create_expected_schema_reload_payload() -> serde_json::Value {
    serde_json::json!({
        "payload": null,
        "errors": [
            {
                "message": "subscription has been closed due to a schema reload",
                "extensions": {
                    "code": "SUBSCRIPTION_SCHEMA_RELOAD"
                }
            }
        ]
    })
}

/// Creates an expected subscription event payload for a configuration reload
fn create_expected_config_reload_payload() -> serde_json::Value {
    serde_json::json!({
        "payload": null,
        "errors": [
            {
                "message": "subscription has been closed due to a configuration reload",
                "extensions": {
                    "code": "SUBSCRIPTION_CONFIG_RELOAD"
                }
            }
        ]
    })
}

/// Creates an expected subscription event payload for the given user number
fn create_expected_user_payload(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "payload": {
            "data": {
                "userWasCreated": {
                    "name": format!("User {}", user_num),
                    "reviews": [{"body": format!("Review {} from user {}", user_num, user_num)}]
                }
            }
        }
    })
}

/// Creates an expected subscription event payload with null userWasCreated (for empty/error payloads)
fn create_expected_null_payload() -> serde_json::Value {
    serde_json::json!({
        "payload": {
            "data": {
                "userWasCreated": null
            }
        }
    })
}

/// Creates an expected subscription event payload for a user with missing reviews field (becomes null)
fn create_expected_user_payload_missing_reviews(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "payload": {
            "data": {
                "userWasCreated": {
                    "name": format!("User {}", user_num),
                    "reviews": null // Missing reviews field gets transformed to null
                }
            }
        }
    })
}

/// Creates an expected subscription event payload for a user with missing reviews field (becomes null) and error
fn create_expected_partial_error_payload(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "payload": {
            "data": {
                "userWasCreated": {
                    "name": format!("User {}", user_num),
                    "reviews": null // Missing reviews field gets transformed to null
                }
            },
            "errors": [
                {
                    "message": "Internal error handling deferred response",
                    "extensions": {
                        "code": "INTERNAL_ERROR"
                    }
                }
            ]
        }
    })
}

/// Creates an expected subscription event payload for a user with missing reviews field (becomes null) and error
fn create_expected_error_payload() -> serde_json::Value {
    serde_json::json!({
        "payload": {
            "data": {
                "userWasCreated": null
            },
            "errors": [{
                "message": "Internal error handling deferred response",
                "extensions": {"code": "INTERNAL_ERROR"}
            }]
        },
    })
}

/// Creates the initial empty subscription response
fn create_initial_empty_response() -> serde_json::Value {
    serde_json::json!({})
}

// Input payload helpers (what we send to the mock WebSocket server)

/// Creates a GraphQL data payload for a user (sent to mock server)
fn create_user_data_payload(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "data": {
            "userWasCreated": {
                "name": format!("User {}", user_num),
                "reviews": [{
                    "body": format!("Review {} from user {}", user_num, user_num)
                }]
            }
        }
    })
}

/// Creates a GraphQL data payload with missing reviews field (sent to mock server)
fn create_user_data_payload_missing_reviews(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "data": {
            "userWasCreated": {
                "name": format!("User {}", user_num)
                // Missing reviews field to test error handling
            }
        },
        "errors": []
    })
}

/// Creates an empty payload (sent to mock server)
fn create_empty_data_payload() -> serde_json::Value {
    serde_json::json!({
        // No data attribute at all
    })
}

/// Creates an expected error response payload (sent to mock server)
fn create_partial_error_payload(user_num: u32) -> serde_json::Value {
    serde_json::json!({
        "data": {
            "userWasCreated": {
                "name": format!("User {}", user_num),
            }
        },
        "errors": [
            {
                "message": "Internal error handling deferred response",
                "extensions": {
                    "code": "INTERNAL_ERROR"
                }
            }
        ]
    })
}

/// Creates an expected error response payload (sent to mock server)
fn create_error_payload() -> serde_json::Value {
    serde_json::json!({
        "data": {
            "userWasCreated": null
        },
        "errors": [
            {
                "message": "Internal error handling deferred response",
                "extensions": {
                    "code": "INTERNAL_ERROR"
                }
            }
        ]
    })
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough(
    #[values(
        SUBSCRIPTION_CONFIG_GRAPHQL_WS,
        SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS
    )]
    config: &str,
) -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));
    // Start subscription server with fixed payloads
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let (_, response) = router.run_subscription(&query).await;

    // Expect the router to handle the subscription successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );

    let stream = response.bytes_stream();
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
    ];
    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs
    router.assert_no_error_logs();

    // Race fix (C6): `is_closed` is set by the mock WS server's close handler
    // in an in-process task; deadline-poll instead of one-shot assert.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough",
    )
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_with_coprocessor() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    // Create fixed payloads for this test (different from first test)
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server and coprocessor
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;
    let coprocessor_server = start_coprocessor_server().await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);
    router.replace_config_string(
        "http://localhost:{{COPROCESSOR_PORT}}",
        &coprocessor_server.uri(),
    );

    info!("WebSocket server started at: {}", ws_url);
    info!(
        "Coprocessor server started at: {}",
        coprocessor_server.uri()
    );

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let (_, response) = router.run_subscription(&query).await;

    // Expect the router to handle the subscription successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );

    let stream = response.bytes_stream();
    // Now we're storing raw responses, so expect the actual multipart response structure
    // First event is an empty object (subscription initialization), followed by data events
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
    ];

    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs (allow expected coprocessor error)
    router.assert_no_error_logs();
    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_with_coprocessor",
    )
    .await;

    Ok(())
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_error_payload(
    #[values(
        SUBSCRIPTION_CONFIG_GRAPHQL_WS,
        SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS
    )]
    config: &str,
) -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    // Create custom payloads: one normal event, one error event (no reviews field)
    let custom_payloads = vec![
        create_user_data_payload(1),
        create_user_data_payload_missing_reviews(2),
    ];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with custom payloads
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

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
    // Now we're storing raw responses, so expect the actual multipart response structure
    // First event is an empty object (subscription initialization), followed by data events
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload_missing_reviews(2),
    ];
    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs
    router.assert_no_error_logs();
    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_error_payload",
    )
    .await;

    Ok(())
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_pure_error_payload(
    #[values(
        SUBSCRIPTION_CONFIG_GRAPHQL_WS,
        SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS
    )]
    config: &str,
) -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    // Create custom payloads: one normal event, one partial error event (data and errors), one pure error event (no data, only errors)
    let custom_payloads = vec![
        create_user_data_payload(1),
        create_partial_error_payload(2),
        create_error_payload(),
    ];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with custom payloads
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

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
    // Now we're storing raw responses, so expect the actual multipart response structure
    // First event is an empty object (subscription initialization), followed by data events
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_partial_error_payload(2),
        create_expected_error_payload(),
    ];
    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs
    router.assert_no_error_logs();
    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_pure_error_payload",
    )
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_pure_error_payload_with_coprocessor()
-> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }
    // Create custom payloads: one normal event, one pure error event (no data, only errors)
    let custom_payloads = vec![
        create_user_data_payload(1),
        create_empty_data_payload(), // Missing required "data" or "errors" field
        create_user_data_payload(2), // This event is received successfully
        create_partial_error_payload(3),
        create_error_payload(),
    ];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server and coprocessor
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;
    let coprocessor_server = start_coprocessor_server().await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);
    router.replace_config_string(
        "http://localhost:{{COPROCESSOR_PORT}}",
        &coprocessor_server.uri(),
    );

    info!("WebSocket server started at: {}", ws_url);
    info!(
        "Coprocessor server started at: {}",
        coprocessor_server.uri()
    );

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

    // Now we're storing raw responses, so expect the actual multipart response structure
    // First event is an empty object (subscription initialization), followed by data events
    // The coprocessor processes all events successfully (router transforms empty payloads to valid GraphQL)
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_null_payload(),
        create_expected_user_payload(2),
        create_expected_partial_error_payload(3),
        create_expected_error_payload(),
    ];
    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs
    router.assert_no_error_logs();
    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_pure_error_payload_with_coprocessor",
    )
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_on_config_reload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with fixed payloads, but do not terminate the connection
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        false,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(include_str!(
            "fixtures/subscription_schema_reload.router.yaml"
        ))
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let (_, response) = router.run_subscription(&query).await;

    // Expect the router to handle the subscription successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );

    let stream = response.bytes_stream();
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
        create_expected_config_reload_payload(),
    ];

    // try to reload the config file
    router.replace_config_string("replaceable", "replaced");

    router.assert_reloaded().await;

    // Race fix (C6): same pattern as Phase 11 site 1 — after
    // `assert_reloaded()` the test scrapes `/metrics` and asserts
    // `total_active + total_terminating == 1`. During reload it transiently
    // sees `active=1, terminating=1` (2 vs 1) because connection bookkeeping
    // is updated asynchronously from the router-side reload notification.
    let sum_metric_counts = |regex: &Regex, metrics: &str| -> usize {
        regex
            .captures_iter(metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}] ([0-9]+)"#)
            .expect("regex");
    let active = Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}] ([0-9]+)"#)
        .expect("regex");
    let metrics = poll_metrics_until(&router, Duration::from_secs(10), |body| {
        let total_active = sum_metric_counts(&active, body);
        let total_terminating = sum_metric_counts(&terminating, body);
        total_active == 1 && total_terminating == 0
    })
    .await;
    let total_active: usize = sum_metric_counts(&active, &metrics);
    let total_terminating: usize = sum_metric_counts(&terminating, &metrics);
    assert_eq!(total_active, 1);
    assert_eq!(total_active + total_terminating, 1);

    verify_subscription_events(stream, expected_events, true).await;

    router.graceful_shutdown().await;
    // router.assert_shutdown().await;

    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");

    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_on_config_reload",
    )
    .await;

    info!(
        "✅ Passthrough subscription mode test completed successfully with {} events",
        custom_payloads.len()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_on_schema_reload() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with fixed payloads, but do not terminate the connection
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        false,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(include_str!(
            "fixtures/subscription_schema_reload.router.yaml"
        ))
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let (_, response) = router.run_subscription(&query).await;

    // Expect the router to handle the subscription successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );

    let stream = response.bytes_stream();
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
        create_expected_schema_reload_payload(),
    ];

    // try to reload the config file
    router.replace_schema_string("createdAt", "created");

    router.assert_reloaded().await;

    // Race fix (C6, Phase 11 site 1): after `assert_reloaded()` the test scrapes
    // `/metrics` and asserts `total_active + total_terminating == 1`. During
    // reload it transiently sees `active=1, terminating=1` (2 vs 1) because
    // connection bookkeeping is updated asynchronously from the router-side
    // reload notification. Deadline-poll the externally observable predicate.
    let sum_metric_counts = |regex: &Regex, metrics: &str| -> usize {
        regex
            .captures_iter(metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}] ([0-9]+)"#)
            .expect("regex");
    let active = Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}] ([0-9]+)"#)
        .expect("regex");
    let metrics = poll_metrics_until(&router, Duration::from_secs(10), |body| {
        let total_active = sum_metric_counts(&active, body);
        let total_terminating = sum_metric_counts(&terminating, body);
        total_active == 1 && total_terminating == 0
    })
    .await;
    let total_active: usize = sum_metric_counts(&active, &metrics);
    let total_terminating: usize = sum_metric_counts(&terminating, &metrics);
    assert_eq!(total_active, 1);
    assert_eq!(total_active + total_terminating, 1);

    verify_subscription_events(stream, expected_events, true).await;

    router.graceful_shutdown().await;
    // router.assert_shutdown().await;

    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");
    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_on_schema_reload",
    )
    .await;

    info!(
        "✅ Passthrough subscription mode test completed successfully with {} events",
        custom_payloads.len()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_dedup() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 50;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with fixed payloads, but do not terminate the connection
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        false,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(include_str!(
            "fixtures/subscription_schema_reload.router.yaml"
        ))
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let ((_, response), (_, response_bis)) = futures::join!(
        router.run_subscription(&query),
        router.run_subscription(&query)
    );

    // Expect the router to handle the subscription successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );
    assert!(
        response_bis.status().is_success(),
        "Subscription request failed with status: {}",
        response_bis.status()
    );

    let stream = response.bytes_stream();

    let stream_bis = response_bis.bytes_stream();

    // Race fix (C6, Phase 11 site 2): subscription counters
    // (`subscriptions_deduplicated` true/false) are incremented in the router
    // after HTTP response headers go out, so a one-shot scrape immediately
    // after both responses succeed races the increment. Deadline-poll until
    // both counters reach 1.
    let sum_metric_counts = |regex: &Regex, metrics: &str| -> usize {
        regex
            .captures_iter(metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };
    let deduplicated_sub =
        Regex::new(r#"(?m)^apollo_router_operations_subscriptions_total[{].+subscriptions_deduplicated="true".+[}] ([0-9]+)"#)
            .expect("regex");
    let duplicated_sub =
        Regex::new(r#"(?m)^apollo_router_operations_subscriptions_total[{].+subscriptions_deduplicated="false".+[}] ([0-9]+)"#)
            .expect("regex");
    let metrics = poll_metrics_until(&router, Duration::from_secs(10), |body| {
        let total_deduplicated_sub = sum_metric_counts(&deduplicated_sub, body);
        let total_duplicated_sub = sum_metric_counts(&duplicated_sub, body);
        total_deduplicated_sub == 1 && total_duplicated_sub == 1
    })
    .await;
    let total_deduplicated_sub: usize = sum_metric_counts(&deduplicated_sub, &metrics);
    assert_eq!(total_deduplicated_sub, 1);
    let total_duplicated_sub: usize = sum_metric_counts(&duplicated_sub, &metrics);
    assert_eq!(total_duplicated_sub, 1);

    // Trick to close the subscription server side
    router.replace_schema_string("createdAt", "created");

    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
        create_expected_schema_reload_payload(),
    ];
    verify_subscription_events(stream, expected_events, true).await;
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
        create_expected_schema_reload_payload(),
    ];
    verify_subscription_events(stream_bis, expected_events, true).await;

    router.graceful_shutdown().await;

    // Race fix (C6): deadline-poll the in-process `is_closed` flag.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_dedup",
    )
    .await;
    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");

    info!(
        "✅ Passthrough subscription mode test completed successfully with {} events",
        custom_payloads.len()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_dedup_close_early() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 50;
    let is_subscription_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with fixed payloads, but do not terminate the connection
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_subscription_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(include_str!(
            "fixtures/subscription_schema_reload.router.yaml"
        ))
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{}/ws", ws_addr);
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());
    let ((_, response), (_, response_bis)) = futures::join!(
        router.run_subscription(&query),
        router.run_subscription(&query)
    );

    // Expect the router to handle both subscriptions successfully
    assert!(
        response.status().is_success(),
        "Subscription request failed with status: {}",
        response.status()
    );
    assert!(
        response_bis.status().is_success(),
        "Subscription request failed with status: {}",
        response_bis.status()
    );

    let metrics = router.get_metrics_response().await?.text().await?;
    let sum_metric_counts = |regex: &Regex| {
        regex
            .captures_iter(&metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };

    let stream = response.bytes_stream();
    let stream_bis = response_bis.bytes_stream();

    // Check that both the original (deduplicated) and the duplicate subscription
    // are reflected in metrics.
    let deduplicated_sub =
        Regex::new(r#"(?m)^apollo_router_operations_subscriptions_total[{].+subscriptions_deduplicated="true".+[}] ([0-9]+)"#)
            .expect("regex");
    let total_deduplicated_sub: usize = sum_metric_counts(&deduplicated_sub);
    assert_eq!(total_deduplicated_sub, 1);
    let duplicated_sub =
        Regex::new(r#"(?m)^apollo_router_operations_subscriptions_total[{].+subscriptions_deduplicated="false".+[}] ([0-9]+)"#)
            .expect("regex");
    let total_duplicated_sub: usize = sum_metric_counts(&duplicated_sub);
    assert_eq!(total_duplicated_sub, 1);

    // We'll start consuming both subscriptions, but cancel the first one as soon as a message is
    // received. the `bis` subscription should continue to receive messages after that.
    let mut multipart = multer::Multipart::new(stream, "graphql");
    let mut multipart_bis = multer::Multipart::new(stream_bis, "graphql");

    // Explicit signal that the primary reader has dropped its multipart stream and is shutting
    // down. Previously this test relied on `task.is_finished()` ordering, which races the tokio
    // scheduler: `break` in the primary task only marks the JoinHandle finished after the runtime
    // gets a chance to poll it again, so the `bis` task could reach the assertion before the
    // primary task had actually been observed as finished. A `Notify` makes the handoff explicit:
    // the primary signals the moment it drops its stream, and the `bis` task waits on that signal
    // before asserting that the primary has fully completed.
    let primary_closed = Arc::new(tokio::sync::Notify::new());
    let primary_closed_signal = primary_closed.clone();

    // Task for the first (deduplicated) subscription.
    let task = tokio::task::spawn(tokio::time::timeout(Duration::from_secs(30), async move {
        let expected_event = create_expected_user_payload(1);
        while let Some(field) = multipart
            .next_field()
            .await
            .expect("could not read next chunk")
        {
            let parsed: serde_json::Value = field.json().await.expect("invalid JSON chunk");
            // Heartbeat
            if parsed == serde_json::json!({}) {
                continue;
            }
            assert_eq!(parsed, expected_event);
            // Close the connection early. The other connection from the duplicate
            // subscription should continue to receive events...
            break;
        }
        // Drop the multipart stream explicitly so the underlying connection is closed before
        // we signal the `bis` task. (Doing this is also what `break` would do implicitly when
        // the async block returns, but being explicit avoids any future refactor accidentally
        // introducing work between the break and the drop.)
        drop(multipart);
        primary_closed_signal.notify_one();
    }));
    // This the the other connection with the duplicate subscription to the one above.
    // After the subscription above is closed, it should continue to receive events.
    let task_bis = tokio::task::spawn(tokio::time::timeout(Duration::from_secs(30), async move {
        let mut expected_events = vec![
            create_expected_user_payload(1),
            create_expected_user_payload(2),
        ];
        while let Some(field) = multipart_bis
            .next_field()
            .await
            .expect("could not read next chunk")
        {
            let parsed: serde_json::Value = field.json().await.expect("invalid JSON chunk");
            // Heartbeat
            if parsed == serde_json::json!({}) {
                continue;
            }
            assert_eq!(parsed, expected_events.remove(0));
        }

        // Make sure that we're actually testing what we think we're testing, i.e. the first task
        // closed its connection successfully. Wait for the explicit signal from the primary task
        // (with a generous timeout in case something has gone wrong) instead of polling
        // `task.is_finished()`, which races the scheduler.
        tokio::time::timeout(Duration::from_secs(30), primary_closed.notified())
            .await
            .expect("primary connection should have signaled close");
        task.await
            .expect("primary task should complete after signaling close")
            .expect("should not have timed out");
        assert!(
            expected_events.is_empty(),
            "should have consumed all events"
        );
    }));

    // If _this_ times out, then chances are that the connection is essentially inert, and the
    // router stopped serving us events on the deduped subscription.
    // See https://github.com/apollographql/router/pull/7879
    task_bis
        .await
        .expect("task should complete")
        .expect("should not have timed out");

    router.graceful_shutdown().await;

    // Check the subscription event listener is closed.
    // Race fix (C6): deadline-poll the in-process flag.
    assert_is_closed_within(
        &is_subscription_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_dedup_close_early",
    )
    .await;
    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");

    info!(
        "✅ Passthrough subscription mode test completed successfully with {} events",
        custom_payloads.len()
    );

    Ok(())
}

/// Test that WebSocket subscriptions work with non-ASCII header values through
/// the full router stack. This validates the fix for the issue where tungstenite could not
/// serialize headers containing non-ASCII (UTF-8) characters like "Montréal".
///
/// This is an end-to-end integration test that verifies the fix works holistically through
/// the router, since axum may be using a different version of tokio-tungstenite.
#[rstest::rstest]
#[tokio::test(flavor = "multi_thread")]
async fn test_subscription_ws_passthrough_with_non_ascii_headers(
    #[values(
        SUBSCRIPTION_CONFIG_GRAPHQL_WS,
        SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS
    )]
    config: &str,
) -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    // Create fixed payloads for consistent testing
    let custom_payloads = vec![create_user_data_payload(1), create_user_data_payload(2)];
    let interval_ms = 10;
    let is_closed = Arc::new(AtomicBool::new(false));

    // Start subscription server with fixed payloads
    let (ws_addr, http_server) = start_subscription_server_with_payloads(
        custom_payloads.clone(),
        interval_ms,
        true,
        is_closed.clone(),
    )
    .await;

    // Create router with port reservations
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Configure URLs using the string replacement method
    let ws_url = format!("ws://{ws_addr}/ws");
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", &http_server.uri());
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", &ws_url);

    info!("WebSocket server started at: {}", ws_url);

    router.start().await;
    router.assert_started().await;

    // Use the configured query that matches our server configuration
    let query = create_sub_query(interval_ms, custom_payloads.len());

    // Create a subscription request with a non-ASCII header
    // The "é" character in "Montréal" is encoded as bytes 0xC3 0xA9 in UTF-8
    let non_ascii_value = "Montréal";
    let response = router
        .execute_query(
            crate::integration::common::Query::builder()
                .body(serde_json::json!({
                    "query": query
                }))
                .headers(std::collections::HashMap::from([
                    (
                        "Accept".to_string(),
                        "multipart/mixed;subscriptionSpec=1.0".to_string(),
                    ),
                    ("x-custom-location".to_string(), non_ascii_value.to_string()),
                ]))
                .build(),
        )
        .await;

    // Expect the router to handle the subscription successfully
    // This is the critical test: the subscription should work with the non-ASCII header.
    // Before the tungstenite fix, this would fail during WebSocket handshake.
    assert!(
        response.1.status().is_success(),
        "Subscription request with non-ASCII header failed with status: {}",
        response.1.status()
    );

    let stream = response.1.bytes_stream();
    let expected_events = vec![
        create_initial_empty_response(),
        create_expected_user_payload(1),
        create_expected_user_payload(2),
    ];
    let _subscription_events = verify_subscription_events(stream, expected_events, true).await;

    // Check for errors in router logs
    router.assert_no_error_logs();

    // Race fix (C6, Phase 11 site 3): `is_closed` is set by the mock WS
    // server's close handler in `tests/integration/subscriptions/mod.rs`,
    // which runs in a separate in-process task. After
    // `verify_subscription_events` returns, the close handler may not yet
    // have observed the close. Deadline-poll the bool.
    assert_is_closed_within(
        &is_closed,
        Duration::from_secs(5),
        "test_subscription_ws_passthrough_with_non_ascii_headers",
    )
    .await;

    info!("WebSocket subscription with non-ASCII headers test completed successfully");

    Ok(())
}
