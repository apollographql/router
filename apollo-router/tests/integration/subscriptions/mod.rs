//! Common subscription testing functionality
use std::net::SocketAddr;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;
use futures::StreamExt;
use serde_json::json;
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
    nb_events: usize,
    interval_ms: u64,
}

pub const SUBSCRIPTION_CONFIG: &str = include_str!("fixtures/subscription.router.yaml");
pub const SUBSCRIPTION_COPROCESSOR_CONFIG: &str =
    include_str!("fixtures/subscription_coprocessor.router.yaml");
pub fn create_sub_query(interval_ms: u64, nb_events: usize) -> String {
    format!(
        r#"subscription {{  userWasCreated(intervalMs: {}, nbEvents: {}) {{    name reviews {{ body }} }}}}"#,
        interval_ms, nb_events
    )
}

pub async fn start_subscription_server_with_config(
    nb_events: usize,
    interval_ms: u64,
) -> (SocketAddr, wiremock::MockServer) {
    let config = SubscriptionServerConfig {
        nb_events,
        interval_ms,
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
        axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    // Wait a moment for the server to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

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

pub async fn verify_subscription_events(
    mut stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    expected_min_events: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let mut subscription_events = Vec::new();
    let mut received_chunks = Vec::new();

    // Set a longer timeout for receiving all events - give plenty of time for all events to arrive
    let timeout = tokio::time::timeout(tokio::time::Duration::from_secs(60), async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Stream error: {}", e))?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            received_chunks.push(chunk_str.to_string());

            debug!("Received chunk: {}", chunk_str);

            // Parse multipart chunks that contain GraphQL data
            if chunk_str.contains("content-type: application/json") {
                debug!("Found JSON chunk, analyzing...");
                debug!("Chunk contains 'User': {}", chunk_str.contains("User"));
                debug!(
                    "Chunk contains 'userWasCreated': {}",
                    chunk_str.contains("userWasCreated")
                );

                // Extract JSON from multipart response
                if let Some(json_start) = chunk_str.find('{') {
                    if let Some(json_end) = chunk_str.rfind('}') {
                        let json_str = &chunk_str[json_start..=json_end];

                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                            // Check for subscription data (could be in "data" or "payload.data")
                            let data = parsed
                                .get("data")
                                .or_else(|| parsed.get("payload").and_then(|p| p.get("data")));

                            if let Some(data) = data {
                                if let Some(user_created) = data.get("userWasCreated") {
                                    subscription_events.push(user_created.clone());

                                    // Verify the structure of each event
                                    if user_created.get("name").is_none() {
                                        return Err(format!(
                                            "Event missing 'name' field: {:?}",
                                            user_created
                                        ));
                                    }
                                    if user_created.get("reviews").is_none() {
                                        return Err(format!(
                                            "Event missing 'reviews' field: {:?}",
                                            user_created
                                        ));
                                    }

                                    info!(
                                        "Received subscription event {}: {:?}",
                                        subscription_events.len(),
                                        user_created
                                    );
                                }
                            }

                            // Check for errors
                            if let Some(errors) = parsed.get("errors") {
                                warn!("Received error in subscription: {:?}", errors);
                            }
                        }
                    }
                }
            }

            // Break when we receive the completion marker, indicating the subscription is finished
            if chunk_str.contains("--graphql--") {
                debug!(
                    "Breaking because found completion marker --graphql-- with {} events",
                    subscription_events.len()
                );
                break;
            }

            // Only break early if we've received more events than expected (safety check)
            if subscription_events.len() > expected_min_events + 5 {
                debug!(
                    "Breaking because received {} events, much more than expected {}",
                    subscription_events.len(),
                    expected_min_events
                );
                break;
            }
        }
        Ok::<(), String>(())
    });

    timeout
        .await
        .map_err(|_| "Subscription test timed out".to_string())??;

    // Verify we received multiple subscription events
    if subscription_events.is_empty() {
        return Err(format!(
            "No subscription events were received. Chunks: {:?}",
            received_chunks
        ));
    }

    if subscription_events.len() < expected_min_events {
        return Err(format!(
            "Expected at least {} subscription events, but received {}. Events: {:?}",
            expected_min_events,
            subscription_events.len(),
            subscription_events
        ));
    }

    debug!("Final event count: {}", subscription_events.len());

    // Verify content of subscription events
    for (i, event) in subscription_events.iter().enumerate() {
        let name = event.get("name").unwrap().as_str().unwrap();
        if !name.starts_with("User ") {
            return Err(format!(
                "Event {} name should start with 'User ', got: {}",
                i + 1,
                name
            ));
        }

        if let Some(reviews) = event.get("reviews").and_then(|r| r.as_array()) {
            if reviews.is_empty() {
                return Err(format!("Event {} reviews should not be empty", i + 1));
            }
            let review_body = reviews[0].get("body").unwrap().as_str().unwrap();
            if !review_body.contains("Review") {
                return Err(format!(
                    "Event {} review should contain 'Review', got: {}",
                    i + 1,
                    review_body
                ));
            }
        }
    }

    // Verify events have different content (proving streaming works)
    if subscription_events.len() > 1 {
        let first_name = subscription_events[0]
            .get("name")
            .unwrap()
            .as_str()
            .unwrap();
        let last_name = subscription_events[subscription_events.len() - 1]
            .get("name")
            .unwrap()
            .as_str()
            .unwrap();

        if first_name == last_name {
            return Err(
                "First and last events should have different names, indicating proper streaming"
                    .to_string(),
            );
        }
    }

    info!(
        "✅ Successfully received {} subscription events",
        subscription_events.len()
    );
    info!("✅ All events contain required fields: name, reviews");
    info!("✅ Events have different content, confirming streaming works");

    Ok(subscription_events)
}

async fn websocket_handler(
    State(config): State<SubscriptionServerConfig>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
) -> Response {
    debug!("WebSocket upgrade requested");
    debug!("Headers: {:?}", headers);
    ws.on_upgrade(move |socket| handle_websocket(socket, config))
}

async fn handle_websocket(mut socket: WebSocket, config: SubscriptionServerConfig) {
    info!("WebSocket connection established");
    while let Some(msg) = socket.recv().await {
        if let Ok(msg) = msg {
            match msg {
                axum::extract::ws::Message::Text(text) => {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                        match parsed.get("type").and_then(|t| t.as_str()) {
                            Some("connection_init") => {
                                // Send connection_ack
                                let ack = json!({
                                    "type": "connection_ack"
                                });
                                if socket
                                    .send(axum::extract::ws::Message::Text(ack.to_string()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Some("start") => {
                                let id = parsed.get("id").and_then(|i| i.as_str()).unwrap_or("1");

                                // Handle subscription
                                if let Some(payload) = parsed.get("payload") {
                                    if let Some(query) =
                                        payload.get("query").and_then(|q| q.as_str())
                                    {
                                        if query.contains("userWasCreated") {
                                            // Use configured values instead of parsing variables
                                            let nb_events = config.nb_events;
                                            let interval_ms = config.interval_ms;

                                            info!(
                                                "Starting subscription with {} events, interval {}ms (configured)",
                                                nb_events, interval_ms
                                            );

                                            // Give the router time to fully establish the subscription stream
                                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                                100,
                                            ))
                                            .await;

                                            // Send multiple subscription events
                                            for i in 1..=nb_events {
                                                let event_data = json!({
                                                    "id": id,
                                                    "type": "data",
                                                    "payload": {
                                                        "data": {
                                                            "userWasCreated": {
                                                                "name": format!("User {}", i),
                                                                "username": format!("user{}", i),
                                                                "reviews": [{
                                                                    "body": format!("Review {} from user {}", i, i)
                                                                }]
                                                            }
                                                        }
                                                    }
                                                });

                                                if socket
                                                    .send(axum::extract::ws::Message::Text(
                                                        event_data.to_string(),
                                                    ))
                                                    .await
                                                    .is_err()
                                                {
                                                    return;
                                                }

                                                debug!(
                                                    "Sent subscription event {}/{}",
                                                    i, nb_events
                                                );

                                                // Wait between events
                                                tokio::time::sleep(
                                                    tokio::time::Duration::from_millis(interval_ms),
                                                )
                                                .await;
                                            }

                                            // Send completion
                                            let complete = json!({
                                                "id": id,
                                                "type": "complete"
                                            });
                                            if socket
                                                .send(axum::extract::ws::Message::Text(
                                                    complete.to_string(),
                                                ))
                                                .await
                                                .is_err()
                                            {
                                                return;
                                            }

                                            info!(
                                                "Completed subscription with {} events",
                                                nb_events
                                            );
                                        }
                                    }
                                }
                            }
                            Some("stop") => {
                                // Handle stop message
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                axum::extract::ws::Message::Close(_) => break,
                _ => {}
            }
        }
    }
}
