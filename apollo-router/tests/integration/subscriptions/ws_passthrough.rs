use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
    router.replace_config_string("rng:", "accounts:");

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

    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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
    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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
    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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
    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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
    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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

    let metrics = router.get_metrics_response().await?.text().await?;
    let sum_metric_counts = |regex: &Regex| {
        regex
            .captures_iter(&metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}] ([0-9]+)"#)
            .expect("regex");
    let total_terminating: usize = sum_metric_counts(&terminating);
    let active = Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}] ([0-9]+)"#)
        .expect("regex");
    let total_active: usize = sum_metric_counts(&active);

    assert_eq!(total_active, 1);
    assert_eq!(total_active + total_terminating, 1);

    verify_subscription_events(stream, expected_events, true).await;

    router.graceful_shutdown().await;
    // router.assert_shutdown().await;

    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");

    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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

    let metrics = router.get_metrics_response().await?.text().await?;
    let sum_metric_counts = |regex: &Regex| {
        regex
            .captures_iter(&metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };
    let terminating =
        Regex::new(r#"(?m)^apollo_router_open_connections[{].+terminating.+[}] ([0-9]+)"#)
            .expect("regex");
    let total_terminating: usize = sum_metric_counts(&terminating);
    let active = Regex::new(r#"(?m)^apollo_router_open_connections[{].+active.+[}] ([0-9]+)"#)
        .expect("regex");
    let total_active: usize = sum_metric_counts(&active);

    assert_eq!(total_active, 1);
    assert_eq!(total_active + total_terminating, 1);

    verify_subscription_events(stream, expected_events, true).await;

    router.graceful_shutdown().await;
    // router.assert_shutdown().await;

    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");
    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));

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
    router.replace_config_string("rng:", "accounts:");

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

    let metrics = router.get_metrics_response().await?.text().await?;
    let sum_metric_counts = |regex: &Regex| {
        regex
            .captures_iter(&metrics)
            .flat_map(|cap| cap.get(1).unwrap().as_str().parse::<usize>())
            .sum()
    };

    let stream = response.bytes_stream();

    let stream_bis = response_bis.bytes_stream();

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

    assert!(is_closed.load(std::sync::atomic::Ordering::Relaxed));
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
    router.replace_config_string("rng:", "accounts:");

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
        // closed its connection successfully
        assert!(task.is_finished(), "primary connection should be closed");
        task.await
            .expect("asserted that it completes")
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
    assert!(is_subscription_closed.load(std::sync::atomic::Ordering::Relaxed));
    // Check for errors in router logs
    router.assert_log_not_contained("connection shutdown exceeded, forcing close");

    info!(
        "✅ Passthrough subscription mode test completed successfully with {} events",
        custom_payloads.len()
    );

    Ok(())
}
