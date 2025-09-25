//! Common subscription testing functionality
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use futures::SinkExt as _;
use futures::StreamExt as _;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use tokio::time::Duration;
use tracing::debug;
use tracing::info;
use tracing::warn;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

pub mod callback;
pub mod ws_passthrough;

#[derive(Clone)]
struct SubscriptionServerConfig {
    payloads: Vec<serde_json::Value>,
    interval_ms: u64,
    complete_subscription: bool,
    is_closed: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackPayload {
    pub kind: String,
    pub action: String,
    pub id: String,
    pub verifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct CallbackTestState {
    pub received_callbacks: Arc<Mutex<Vec<CallbackPayload>>>,
    pub subscription_ids: Arc<Mutex<Vec<String>>>,
}

impl Default for CallbackTestState {
    fn default() -> Self {
        Self {
            received_callbacks: Arc::new(Mutex::new(Vec::new())),
            subscription_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub const SUBSCRIPTION_CONFIG_SUBSCRIPTIONS_TRANSPORT_WS: &str =
    include_str!("fixtures/subscription.router.yaml");
pub const SUBSCRIPTION_CONFIG_GRAPHQL_WS: &str =
    include_str!("fixtures/subscription_graphql_ws.router.yaml");
pub const SUBSCRIPTION_COPROCESSOR_CONFIG: &str =
    include_str!("fixtures/subscription_coprocessor.router.yaml");
pub const CALLBACK_CONFIG: &str = include_str!("fixtures/callback.router.yaml");
pub fn create_sub_query(interval_ms: u64, nb_events: usize) -> String {
    format!(
        r#"subscription {{  userWasCreated(intervalMs: {interval_ms}, nbEvents: {nb_events}) {{    name reviews {{ body }} }}}}"#
    )
}

/// Set up a WebSocket server that sends the given JSON payloads to clients over either of the
/// GraphQL-over-WS subscription protocols.
///
/// # Parameters
/// - `interval_ms` - time between subscription events
/// - `complete_subscription` - make the server immediately close the subscription when all events
/// are sent. If `false`, the subscription remains open, which is useful for testing
/// client-initiated closing.
/// - `is_closed` - the server sets this to `true` when any WS connection has closed
pub async fn start_subscription_server_with_payloads(
    payloads: Vec<serde_json::Value>,
    interval_ms: u64,
    complete_subscription: bool,
    is_closed: Arc<AtomicBool>,
) -> (SocketAddr, wiremock::MockServer) {
    let config = SubscriptionServerConfig {
        payloads,
        interval_ms,
        complete_subscription,
        is_closed,
    };

    // Start WebSocket server using axum
    let app = Router::new()
        .route("/ws", get(websocket_handler))
        .route("/", get(|| async { "WebSocket server running" }))
        .fallback(|uri: axum::http::Uri| async move {
            debug!("Fallback route hit: {}", uri);
            "Not found"
        })
        .with_state(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        info!("Starting axum WebSocket server...");
        axum::serve(listener, app).await.unwrap();
    });

    // Wait a moment for the server to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    info!("Axum server running on {}", ws_addr);

    // Start HTTP mock server for regular GraphQL queries
    let http_server = wiremock::MockServer::start().await;

    // Mock regular GraphQL queries (non-subscription)
    Mock::given(method("POST"))
        .respond_with(|req: &wiremock::Request| {
            let body = req
                .body_json::<serde_json::Value>()
                .unwrap_or_else(|_| json!({}));

            if let Some(query) = body.get("query").and_then(|q| q.as_str()) {
                // Don't handle subscriptions here - they go through WebSocket
                if !query.contains("subscription") {
                    return ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "_entities": [{
                                "name": "Test User",
                                "username": "testuser"
                            }]
                        }
                    }));
                }
            }

            // For subscription queries over HTTP, redirect to WebSocket
            ResponseTemplate::new(400).set_body_json(json!({
                "errors": [{
                    "message": "Subscriptions must use WebSocket"
                }]
            }))
        })
        .mount(&http_server)
        .await;

    (ws_addr, http_server)
}

pub async fn start_coprocessor_server() -> wiremock::MockServer {
    let coprocessor_server = wiremock::MockServer::start().await;

    // Create a coprocessor that echoes back what it receives
    Mock::given(method("POST"))
        .respond_with(|req: &wiremock::Request| {
            // Echo back the request body as the response
            let body = req.body.clone();
            debug!(
                "Coprocessor received request: {}",
                String::from_utf8_lossy(&body)
            );

            ResponseTemplate::new(200)
                .set_body_bytes(body)
                .append_header("content-type", "application/json")
        })
        .mount(&coprocessor_server)
        .await;

    info!(
        "Coprocessor server started at: {}",
        coprocessor_server.uri()
    );
    coprocessor_server
}

fn is_json_field(field: &multer::Field<'_>) -> bool {
    field
        .content_type()
        .is_some_and(|mime| mime.essence_str() == "application/json")
}

pub async fn verify_subscription_events(
    stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send,
    expected_events: Vec<serde_json::Value>,
    include_heartbeats: bool,
) -> Vec<serde_json::Value> {
    use pretty_assertions::assert_eq;

    // Use `multipart/form-data` parsing. The router actually responds with `multipart/mixed`, but
    // the formats are compatible.
    let mut multipart = multer::Multipart::new(stream, "graphql");

    let mut subscription_events = Vec::new();
    // Set a longer timeout for receiving all events
    let timeout = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(field) = multipart
            .next_field()
            .await
            .expect("could not read next chunk")
        {
            assert!(is_json_field(&field), "all response chunks must be JSON");

            let parsed: serde_json::Value = field.json().await.expect("invalid JSON chunk");
            if parsed == serde_json::json!({}) && !include_heartbeats {
                continue;
            }

            subscription_events.push(parsed);
        }

        // If we've received more events than expected, that's an error
        assert!(
            subscription_events.len() <= expected_events.len(),
            "Received {} events but only expected {}. Extra events should not arrive after termination.\nUnexpected event: {}",
            subscription_events.len(),
            expected_events.len(),
            subscription_events.last().unwrap(),
        );
    });

    timeout.await.expect("Subscription test timed out");
    assert!(
        subscription_events.len() == expected_events.len(),
        "Received {} events but expected {}. Stream may have terminated early.",
        subscription_events.len(),
        expected_events.len()
    );

    // Give the stream a moment to ensure it's properly terminated and no more events arrive
    let termination_timeout = tokio::time::timeout(Duration::from_millis(1000), async {
        while let Some(field) = multipart
            .next_field()
            .await
            .expect("could not read next chunk")
        {
            assert!(is_json_field(&field), "all response chunks must be JSON");

            let parsed: serde_json::Value = field.json().await.expect("invalid JSON chunk");
            let data = parsed
                .get("data")
                .or_else(|| parsed.get("payload").and_then(|p| p.get("data")));

            assert!(
                data.is_none(),
                "Unexpected additional event received after {} expected events: {}",
                expected_events.len(),
                parsed
            );
        }
    });

    assert!(
        termination_timeout.await.is_ok(),
        "subscription should have closed cleanly"
    );
    // Simple equality comparison using pretty_assertions
    assert_eq!(
        subscription_events, expected_events,
        "Subscription events do not match expected events"
    );

    subscription_events
}

async fn websocket_handler(
    State(config): State<SubscriptionServerConfig>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
) -> Response {
    debug!("WebSocket upgrade requested");
    debug!("Headers: {:?}", headers);
    // Speak both protocols
    ws.protocols(["graphql-ws", "graphql-transport-ws"])
        .on_upgrade(async move |socket| {
            match socket
                .protocol()
                .expect("must have been provided due to `ws.protocols()` call")
                .as_bytes()
            {
                b"graphql-transport-ws" => handle_websocket_modern(socket, config).await,
                b"graphql-ws" => handle_websocket_legacy(socket, config).await,
                _ => unreachable!("other protocols rejected by `ws.protocols()` call"),
            }
        })
}

/// Create a WebSocket message from a JSON value.
fn json_message<T: serde::Serialize>(data: &T) -> axum::extract::ws::Message {
    axum::extract::ws::Message::text(serde_json::to_string(data).unwrap())
}

/// Handle a WebSocket connection according to the legacy protocol described in:
/// https://github.com/apollographql/subscriptions-transport-ws/blob/36f3f6f780acc1a458b768db13fd39c65e5e6518/PROTOCOL.md
///
/// Note this is a subgraph server, and its purpose is to validate that the router speaks the
/// right protocol. For this reason, it has strict assertions throughout.
async fn handle_websocket_legacy(socket: WebSocket, config: SubscriptionServerConfig) {
    info!("WebSocket connection established");

    let mut subscriptions = HashMap::new();

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (message_sender, mut message_receiver) = tokio::sync::mpsc::channel(10);

    // We need a bit of indirection to be able to send messages on the socket from individual
    // subscription tasks, because the socket is not `Clone`
    tokio::task::spawn(async move {
        while let Some(message) = message_receiver.recv().await {
            ws_sender
                .send(message)
                .await
                .expect("could not send message from subgraph to router");
        }
    });

    while let Some(msg) = ws_receiver.next().await {
        match msg.expect("error receiving websocket message from the router") {
            axum::extract::ws::Message::Text(text) => {
                let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                    panic!("router sent non-JSON subscription message: {text}");
                };

                match parsed.get("type").and_then(|t| t.as_str()) {
                    Some("connection_init") => {
                        // Client sends this message after plain websocket connection to start the communication with the server
                        // The server will respond only with `connection_ack` + `ka` (if used) or `connection_error` to this message.

                        let ack = json!({ "type": "connection_ack" });
                        message_sender
                            .send(json_message(&ack))
                            .await
                            .expect("router already closed the connection");
                    }
                    Some("start") => {
                        // Client sends this message to execute GraphQL operation
                        // - `id: string` : The id of the GraphQL operation to start
                        // - `payload: Object`:
                        //    * `query: string` : GraphQL operation as string or parsed GraphQL document node
                        //    * `variables?: Object` : Object with GraphQL variables
                        //    * `operationName?: string` : GraphQL operation name
                        let id = parsed
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("1")
                            .to_string();

                        let Some(query) = parsed
                            .get("payload")
                            .and_then(|payload| payload.get("query"))
                            .and_then(|query| query.as_str())
                        else {
                            panic!(r#"router sent invalid "start" message: {parsed}"#);
                        };

                        // actual implementation for our test subscription
                        if !query.contains("userWasCreated") {
                            unimplemented!("only the userWasCreated subscription is supported");
                        }

                        let (subscription_close_tx, mut subscription_close_rx) =
                            tokio::sync::oneshot::channel::<()>();
                        assert!(
                            subscriptions
                                .insert(id.clone(), subscription_close_tx)
                                .is_none(),
                            "received duplicate subscription id={id}"
                        );

                        let interval_ms = config.interval_ms;
                        let payloads = config.payloads.clone();
                        let message_sender = message_sender.clone();
                        tokio::task::spawn(async move {
                            info!(
                                "Starting subscription with {} events, interval {}ms (configured)",
                                payloads.len(),
                                interval_ms
                            );

                            // Send multiple subscription events
                            let mut i = 0;
                            for custom_payload in &payloads {
                                // Wait between events
                                tokio::select! {
                                    _ = &mut subscription_close_rx => {
                                        debug!("Client stopping subscription early");
                                        break;
                                    }
                                    _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {}
                                }

                                // Always send exactly what we're given - no transformation
                                let event_data = json!({
                                    "id": id,
                                    "type": "data",
                                    "payload": custom_payload
                                });

                                if message_sender
                                    .send(json_message(&event_data))
                                    .await
                                    .is_err()
                                {
                                    // This could be a benign race condition _or_ something that
                                    // causes a test to fail, so let's at least say something
                                    warn!(
                                        "Router already closed connection while server tried to send a message"
                                    );
                                }

                                i += 1;
                                debug!("Sent subscription event {}/{}", i, payloads.len());
                            }

                            if config.complete_subscription {
                                // Send completion
                                // TODO(@goto-bus-stop): only when client did not proactively close
                                // subscription
                                let complete = json!({
                                    "id": id,
                                    "type": "complete"
                                });
                                if message_sender.send(json_message(&complete)).await.is_err() {
                                    // This could be a benign race condition _or_ something that
                                    // causes a test to fail, so let's at least say something
                                    warn!(
                                        "Router already closed connection while server tried to complete subscription"
                                    );
                                }

                                info!("Completed subscription with {i} events");
                            } else {
                                info!(
                                    "Sent {i} subscription events but did not send `complete` message"
                                );
                            }
                        });
                    }
                    Some("stop") => {
                        // Client sends this message in order to stop a running GraphQL operation execution (for example: unsubscribe)
                        // Multiple subscriptions can exist on a single connection in theory so we cannot just close the connection entirely
                        let id = parsed.get("id").and_then(|i| i.as_str()).unwrap_or("1");

                        if let Some(tx) = subscriptions.remove(id) {
                            _ = tx.send(());
                        }
                    }
                    Some("connection_terminate") => {
                        // Client sends this message to terminate the connection.

                        assert!(
                            subscriptions.is_empty(),
                            "router did not close subscriptions cleanly: {:?}",
                            subscriptions.keys().collect::<Vec<_>>()
                        );
                        break;
                    }
                    ty => panic!("router sent unexpected message type: {ty:?}"),
                }
            }
            axum::extract::ws::Message::Close(_) => {
                panic!(
                    "router should not unilaterally close connection, but send `connection_terminate` message"
                );
            }
            _ => {}
        }
    }

    // Tests can assert this to know if the server closed the connection cleanly
    config
        .is_closed
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Handle a WebSocket connection according to the modern protocol.
/// Spec URL: https://github.com/enisdenjo/graphql-ws/blob/0c0eb499c3a0278c6d9cc799064f22c5d24d2f60/PROTOCOL.md
async fn handle_websocket_modern(socket: WebSocket, config: SubscriptionServerConfig) {
    info!("WebSocket connection established");

    let mut subscriptions = HashMap::new();

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (message_sender, mut message_receiver) = tokio::sync::mpsc::channel(10);

    // We need a bit of indirection to be able to send messages on the socket from individual
    // subscription tasks, because the socket is not `Clone`
    tokio::task::spawn(async move {
        while let Some(message) = message_receiver.recv().await {
            ws_sender
                .send(message)
                .await
                .expect("could not send message from subgraph to router");
        }
    });

    while let Some(msg) = ws_receiver.next().await {
        match msg.expect("error receiving websocket message from the router") {
            axum::extract::ws::Message::Text(text) => {
                let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                    panic!("router sent non-JSON subscription message: {text}");
                };

                match parsed.get("type").and_then(|t| t.as_str()) {
                    Some("connection_init") => {
                        // Client sends this message after plain websocket connection to start the communication with the server
                        // The server will respond only with `connection_ack` + `ka` (if used) or `connection_error` to this message.

                        let ack = json!({ "type": "connection_ack" });
                        message_sender
                            .send(json_message(&ack))
                            .await
                            .expect("router already closed the connection");
                    }
                    Some("ping") => {
                        let pong = json!({ "type": "pong" });
                        message_sender
                            .send(json_message(&pong))
                            .await
                            .expect("router already closed the connection");
                    }
                    Some("subscribe") => {
                        // Requests an operation specified in the message payload.
                        // This message provides a unique ID field to connect published messages to the operation requested by this message.
                        let id = parsed
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("1")
                            .to_string();

                        let Some(query) = parsed
                            .get("payload")
                            .and_then(|payload| payload.get("query"))
                            .and_then(|query| query.as_str())
                        else {
                            panic!(r#"router sent invalid "start" message: {parsed}"#);
                        };

                        if !query.contains("userWasCreated") {
                            unimplemented!("only the userWasCreated subscription is supported");
                        }

                        let (subscription_close_tx, mut subscription_close_rx) =
                            tokio::sync::oneshot::channel::<()>();
                        assert!(
                            subscriptions
                                .insert(id.clone(), subscription_close_tx)
                                .is_none(),
                            "received duplicate subscription id={id}"
                        );

                        let interval_ms = config.interval_ms;
                        let payloads = config.payloads.clone();
                        let message_sender = message_sender.clone();
                        // actual implementation for our test subscription
                        tokio::task::spawn(async move {
                            info!(
                                "Starting subscription with {} events, interval {}ms (configured)",
                                payloads.len(),
                                interval_ms
                            );

                            // Send multiple subscription events
                            let mut i = 0;
                            for custom_payload in &payloads {
                                // Wait between events
                                tokio::select! {
                                    _ = &mut subscription_close_rx => {
                                        debug!("Client stopping subscription early");
                                        break;
                                    }
                                    _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {}
                                }

                                // Always send exactly what we're given - no transformation
                                let event_data = json!({
                                    "id": id,
                                    "type": "next",
                                    "payload": custom_payload
                                });

                                if message_sender
                                    .send(json_message(&event_data))
                                    .await
                                    .is_err()
                                {
                                    // This could be a benign race condition _or_ something that
                                    // causes a test to fail, so let's at least say something
                                    warn!(
                                        "Router already closed connection while server tried to send a message"
                                    );
                                }

                                i += 1;
                                debug!("Sent subscription event {}/{}", i, payloads.len());
                            }

                            if config.complete_subscription {
                                // Send completion
                                // TODO(@goto-bus-stop): only when client did not proactively close
                                // subscription
                                let complete = json!({
                                    "id": id,
                                    "type": "complete"
                                });
                                if message_sender.send(json_message(&complete)).await.is_err() {
                                    // This could be a benign race condition _or_ something that
                                    // causes a test to fail, so let's at least say something
                                    warn!(
                                        "Router already closed connection while server tried to complete subscription"
                                    );
                                }

                                info!("Completed subscription with {i} events");
                            } else {
                                info!(
                                    "Sent {i} subscription events but did not send `complete` message"
                                );
                            }
                        });
                    }
                    Some("complete") => {
                        // Multiple subscriptions can exist on a single connection in theory so we cannot just close the connection entirely
                        let id = parsed.get("id").and_then(|i| i.as_str()).unwrap_or("1");

                        if let Some(tx) = subscriptions.remove(id) {
                            _ = tx.send(());
                        }
                    }
                    ty => panic!("router sent unexpected message type: {ty:?}"),
                }
            }
            axum::extract::ws::Message::Close(_) => break,
            _ => {}
        }
    }

    assert!(
        subscriptions.is_empty(),
        "router did not close subscriptions cleanly: {:?}",
        subscriptions.keys().collect::<Vec<_>>()
    );
    config
        .is_closed
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

pub async fn start_callback_server() -> (SocketAddr, CallbackTestState) {
    let state = CallbackTestState::default();
    let app_state = state.clone();

    let app = Router::new()
        .route("/callback/{id}", post(handle_callback))
        .route("/callback", post(handle_callback_no_id))
        .route("/", get(|| async { "Callback server running" }))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        info!("Starting callback server...");
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    info!("Callback server running on {}", addr);

    (addr, state)
}

async fn handle_callback(
    State(state): State<CallbackTestState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    axum::extract::Json(payload): axum::extract::Json<CallbackPayload>,
) -> StatusCode {
    debug!("Received callback for subscription {}: {:?}", id, payload);
    debug!("Headers: {:?}", headers);

    if payload.id != id {
        warn!("ID mismatch: URL={}, payload={}", id, payload.id);
        return StatusCode::BAD_REQUEST;
    }

    {
        let mut callbacks = state.received_callbacks.lock();
        callbacks.push(payload.clone());
    }

    match payload.action.as_str() {
        "check" => {
            let ids = state.subscription_ids.lock();
            if ids.contains(&payload.id) {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
        "next" | "complete" => {
            let ids = state.subscription_ids.lock();
            if ids.contains(&payload.id) {
                if payload.action == "next" {
                    StatusCode::OK
                } else {
                    StatusCode::ACCEPTED
                }
            } else {
                StatusCode::NOT_FOUND
            }
        }
        "heartbeat" => {
            let ids = state.subscription_ids.lock();
            let all_valid = payload
                .ids
                .as_ref()
                .is_none_or(|callback_ids| callback_ids.iter().all(|id| ids.contains(id)));

            if all_valid {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

async fn handle_callback_no_id(
    State(state): State<CallbackTestState>,
    headers: HeaderMap,
    axum::extract::Json(payload): axum::extract::Json<CallbackPayload>,
) -> StatusCode {
    debug!("Received callback without ID: {:?}", payload);
    debug!("Headers: {:?}", headers);

    {
        let mut callbacks = state.received_callbacks.lock();
        callbacks.push(payload.clone());
    }

    match payload.action.as_str() {
        "heartbeat" => StatusCode::NO_CONTENT,
        _ => StatusCode::BAD_REQUEST,
    }
}

pub async fn start_callback_subgraph_server(
    nb_events: usize,
    interval_ms: u64,
    callback_url: String,
) -> wiremock::MockServer {
    start_callback_subgraph_server_with_payloads(
        generate_default_payloads(nb_events),
        interval_ms,
        callback_url,
    )
    .await
}

pub async fn start_callback_subgraph_server_with_payloads(
    payloads: Vec<serde_json::Value>,
    interval_ms: u64,
    callback_url: String,
) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(move |req: &wiremock::Request| {
            let body = req
                .body_json::<serde_json::Value>()
                .unwrap_or_else(|_| json!({}));

            if let Some(query) = body.get("query").and_then(|q| q.as_str()) {
                if query.contains("subscription") && query.contains("userWasCreated") {
                    let extensions = body.get("extensions");
                    let subscription_ext = extensions.and_then(|e| e.get("subscription"));

                    if let Some(sub_ext) = subscription_ext {
                        let subscription_id = sub_ext
                            .get("subscriptionId")
                            .and_then(|id| id.as_str())
                            .unwrap_or("test-sub-id");
                        let callback_url = sub_ext
                            .get("callbackUrl")
                            .and_then(|url| url.as_str())
                            .unwrap_or(&callback_url);

                        info!(
                            "Subgraph received subscription request with callback URL: {}",
                            callback_url
                        );
                        info!("Subscription ID: {}", subscription_id);

                        tokio::spawn(send_callback_events_with_payloads(
                            callback_url.to_string(),
                            subscription_id.to_string(),
                            payloads.clone(),
                            interval_ms,
                        ));

                        return ResponseTemplate::new(200).set_body_json(json!({
                            "data": {
                                "userWasCreated": null
                            }
                        }));
                    }
                }

                return ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "_entities": [{
                            "name": "Test User",
                            "username": "testuser"
                        }]
                    }
                }));
            }

            ResponseTemplate::new(400).set_body_json(json!({
                "errors": [{
                    "message": "Invalid request"
                }]
            }))
        })
        .mount(&server)
        .await;

    info!("Callback subgraph server started at: {}", server.uri());
    server
}

pub fn generate_default_payloads(nb_events: usize) -> Vec<serde_json::Value> {
    (1..=nb_events)
        .map(|i| {
            json!({
                "data": {
                    "userWasCreated": {
                        "name": format!("User {}", i),
                        "reviews": [{
                            "body": format!("Review {} from user {}", i, i)
                        }]
                    }
                }
            })
        })
        .collect()
}

async fn send_callback_events_with_payloads(
    callback_url: String,
    subscription_id: String,
    payloads: Vec<serde_json::Value>,
    interval_ms: u64,
) {
    let client = reqwest::Client::new();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for (i, custom_payload) in payloads.iter().enumerate() {
        let payload = CallbackPayload {
            kind: "subscription".to_string(),
            action: "next".to_string(),
            id: subscription_id.clone(),
            verifier: "test-verifier".to_string(),
            payload: Some(custom_payload.clone()),
            errors: None,
            ids: None,
        };

        let response = client.post(&callback_url).json(&payload).send().await;

        match response {
            Ok(resp) => debug!(
                "Sent callback event {}/{}, status: {}",
                i + 1,
                payloads.len(),
                resp.status()
            ),
            Err(e) => warn!("Failed to send callback event {}: {}", i + 1, e),
        }

        if i < payloads.len() - 1 {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    let complete_payload = CallbackPayload {
        kind: "subscription".to_string(),
        action: "complete".to_string(),
        id: subscription_id.clone(),
        verifier: "test-verifier".to_string(),
        payload: None,
        errors: None,
        ids: None,
    };

    let response = client
        .post(&callback_url)
        .json(&complete_payload)
        .send()
        .await;

    match response {
        Ok(resp) => info!("Sent completion callback, status: {}", resp.status()),
        Err(e) => warn!("Failed to send completion callback: {}", e),
    }
}
