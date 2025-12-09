//! Integration tests for WebSocket trace propagation
//!
//! These tests verify that trace context (Datadog, W3C, etc.) is properly
//! propagated to subgraphs during WebSocket subscription connections.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use parking_lot::Mutex;
use serde_json::json;
use tokio::time::Duration;
use tower::BoxError;
use tracing::info;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Telemetry;
use crate::integration::common::graph_os_enabled;

/// Shared state to capture WebSocket upgrade request headers
#[derive(Clone)]
struct HeaderCaptureState {
    captured_headers: Arc<Mutex<Option<HeaderMap>>>,
}

impl HeaderCaptureState {
    fn new() -> Self {
        Self {
            captured_headers: Arc::new(Mutex::new(None)),
        }
    }

    fn get_captured_headers(&self) -> Option<HeaderMap> {
        self.captured_headers.lock().clone()
    }
}

/// WebSocket handler that captures upgrade request headers
async fn websocket_handler_with_header_capture(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<HeaderCaptureState>,
) -> Response {
    // Capture the headers from the upgrade request
    *state.captured_headers.lock() = Some(headers.clone());
    info!("Captured WebSocket upgrade headers: {:?}", headers);

    ws.on_upgrade(handle_websocket)
}

/// Handle the WebSocket connection after upgrade
async fn handle_websocket(mut socket: WebSocket) {
    use axum::extract::ws::Message;

    // Wait for incoming messages
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Text(text)) => {
                info!("Received WebSocket message: {}", text);

                // Parse the message
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                    && let Some(msg_type) = value.get("type").and_then(|t| t.as_str())
                {
                    match msg_type {
                        "connection_init" => {
                            // Send connection_ack
                            let ack = json!({"type": "connection_ack"});
                            if socket
                                .send(Message::Text(ack.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        "subscribe" => {
                            // Send a subscription response
                            let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("1");
                            let response = json!({
                                "id": id,
                                "type": "next",
                                "payload": {
                                    "data": {
                                        "userWasCreated": {
                                            "name": "Test User",
                                            "reviews": [{"body": "Test Review"}]
                                        }
                                    }
                                }
                            });
                            if socket
                                .send(Message::Text(response.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }

                            // Send complete
                            let complete = json!({"id": id, "type": "complete"});
                            if socket
                                .send(Message::Text(complete.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        "complete" => {
                            // Client is closing the subscription
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket connection closed");
                break;
            }
            _ => {}
        }
    }
}

/// Start a WebSocket server that captures upgrade request headers
async fn start_header_capturing_ws_server() -> (SocketAddr, HeaderCaptureState) {
    let state = HeaderCaptureState::new();

    let app = Router::new()
        .route("/ws", get(websocket_handler_with_header_capture))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        info!("Starting header-capturing WebSocket server on {}", ws_addr);
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for server to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    (ws_addr, state)
}

/// Test that Datadog trace headers are propagated in WebSocket upgrade requests
#[tokio::test(flavor = "multi_thread")]
async fn test_datadog_trace_headers_in_websocket_upgrade() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped - graph_os not enabled");
        return Ok(());
    }

    // Start a WebSocket server that captures headers
    let (ws_addr, header_state) = start_header_capturing_ws_server().await;

    // Create router config with Datadog telemetry
    let config = r#"
subscription:
  enabled: true
  mode:
    passthrough:
      all:
        path: /ws
      subgraphs:
        accounts:
          path: /ws
          protocol: graphql_ws
telemetry:
  exporters:
    tracing:
      propagation:
        datadog: true
      datadog:
        enabled: true
override_subgraph_url:
  accounts: http://localhost:{{SUBGRAPH_PORT}}
"#;

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Datadog)
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Override the subgraph URL to point to our capturing WebSocket server
    router.set_address_from_uri("SUBGRAPH_PORT", &format!("http://{}", ws_addr));

    router.start().await;
    router.assert_started().await;

    // Execute a subscription
    let subscription_query = r#"subscription { userWasCreated { name reviews { body } } }"#;
    let (_id, response) = router.run_subscription(subscription_query).await;

    // Verify the subscription started successfully
    assert!(
        response.status().is_success(),
        "Subscription should start successfully, status: {}",
        response.status()
    );

    // Give the WebSocket connection time to complete the upgrade
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Verify that headers were captured
    let captured_headers = header_state.get_captured_headers();
    assert!(
        captured_headers.is_some(),
        "WebSocket upgrade request headers should have been captured"
    );

    let headers = captured_headers.unwrap();
    info!("Captured headers: {:?}", headers);

    // Verify Datadog trace headers are present
    assert!(
        headers.contains_key("x-datadog-trace-id"),
        "x-datadog-trace-id header should be present in WebSocket upgrade request. Headers: {:?}",
        headers
    );

    assert!(
        headers.contains_key("x-datadog-parent-id"),
        "x-datadog-parent-id header should be present in WebSocket upgrade request. Headers: {:?}",
        headers
    );

    assert!(
        headers.contains_key("x-datadog-sampling-priority"),
        "x-datadog-sampling-priority header should be present in WebSocket upgrade request. Headers: {:?}",
        headers
    );

    // Verify the trace ID is in decimal format (Datadog's format)
    if let Some(trace_id) = headers.get("x-datadog-trace-id") {
        let trace_id_str = trace_id.to_str().unwrap();
        assert!(
            trace_id_str.parse::<u64>().is_ok(),
            "Datadog trace ID should be in decimal format, got: {}",
            trace_id_str
        );
        // Should not contain hex characters a-f
        assert!(
            !trace_id_str
                .chars()
                .any(|c| matches!(c, 'a'..='f' | 'A'..='F')),
            "Datadog trace ID should not contain hex characters, got: {}",
            trace_id_str
        );
    }

    // Verify the parent ID is in decimal format
    if let Some(parent_id) = headers.get("x-datadog-parent-id") {
        let parent_id_str = parent_id.to_str().unwrap();
        assert!(
            parent_id_str.parse::<u64>().is_ok(),
            "Datadog parent ID should be in decimal format, got: {}",
            parent_id_str
        );
    }

    // Verify sampling priority is a valid integer
    if let Some(priority) = headers.get("x-datadog-sampling-priority") {
        let priority_str = priority.to_str().unwrap();
        assert!(
            priority_str.parse::<i32>().is_ok(),
            "Datadog sampling priority should be a valid integer, got: {}",
            priority_str
        );
    }

    router.graceful_shutdown().await;
    Ok(())
}

/// Test that W3C TraceContext headers are propagated in WebSocket upgrade requests
#[tokio::test(flavor = "multi_thread")]
async fn test_w3c_trace_headers_in_websocket_upgrade() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped - graph_os not enabled");
        return Ok(());
    }

    // Start a WebSocket server that captures headers
    let (ws_addr, header_state) = start_header_capturing_ws_server().await;

    // Create router config with W3C propagation
    let config = r#"
subscription:
  enabled: true
  mode:
    passthrough:
      all:
        path: /ws
      subgraphs:
        accounts:
          path: /ws
          protocol: graphql_ws
telemetry:
  exporters:
    tracing:
      propagation:
        trace_context: true
override_subgraph_url:
  accounts: http://localhost:{{SUBGRAPH_PORT}}
"#;

    let mut router = IntegrationTest::builder()
        .telemetry(Telemetry::Otlp { endpoint: None })
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(config)
        .build()
        .await;

    // Override the subgraph URL to point to our capturing WebSocket server
    router.set_address_from_uri("SUBGRAPH_PORT", &format!("http://{}", ws_addr));

    router.start().await;
    router.assert_started().await;

    // Execute a subscription
    let subscription_query = r#"subscription { userWasCreated { name reviews { body } } }"#;
    let (_id, response) = router.run_subscription(subscription_query).await;

    // Verify the subscription started successfully
    assert!(
        response.status().is_success(),
        "Subscription should start successfully, status: {}",
        response.status()
    );

    // Give the WebSocket connection time to complete the upgrade
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Verify that headers were captured
    let captured_headers = header_state.get_captured_headers();
    assert!(
        captured_headers.is_some(),
        "WebSocket upgrade request headers should have been captured"
    );

    let headers = captured_headers.unwrap();
    info!("Captured headers: {:?}", headers);

    // Verify W3C traceparent header is present
    assert!(
        headers.contains_key("traceparent"),
        "traceparent header should be present in WebSocket upgrade request. Headers: {:?}",
        headers
    );

    // Verify traceparent format: 00-{32 hex}-{16 hex}-{2 hex}
    if let Some(traceparent) = headers.get("traceparent") {
        let traceparent_str = traceparent.to_str().unwrap();
        assert!(
            traceparent_str.starts_with("00-"),
            "traceparent should start with version 00, got: {}",
            traceparent_str
        );
        assert!(
            traceparent_str.len() >= 55, // 00-{32}-{16}-{2} = 55 characters minimum
            "traceparent should have correct length (at least 55 chars), got length: {}",
            traceparent_str.len()
        );
    }

    router.graceful_shutdown().await;
    Ok(())
}
