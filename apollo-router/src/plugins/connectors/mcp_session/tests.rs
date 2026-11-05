use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use http::StatusCode;
use http::header::AUTHORIZATION;
use parking_lot::Mutex;
use serde_json::Value;
use serde_json::json;
use tokio::sync::Notify;
use tower::BoxError;
use tower::Layer;
use tower::ServiceExt;

use super::McpSessionLayer;
use super::PROTOCOL_VERSION;
use super::SESSION_ID;
use crate::Context;
use crate::metrics::FutureMetricsExt as _;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::connectors::configuration::McpSourceConfiguration;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router::body;

/// A request as the fake server saw it.
#[derive(Clone, Debug)]
struct Seen {
    method: Method,
    rpc_method: Option<String>,
    rpc_id: Option<Value>,
    session: Option<String>,
    version: Option<String>,
    authorization: Option<String>,
    /// Whether the request still carried the [`Marker`] extension its connector request had,
    /// as SigV4 signing parameters must be for the request to be signed below this layer
    marked: bool,
}

/// A request extension standing in for SigV4 signing parameters, which travel the same way.
#[derive(Clone)]
struct Marker;

/// A fake MCP server that requires sessions, as servers built on the stateful SDK defaults do.
#[derive(Default)]
struct Server {
    stateless: bool,
    event_stream: bool,
    refuse_initialize: bool,
    /// Refuse a tool call with 401 when it carries other credentials than its session was
    /// opened with, as servers that tie sessions to tokens do
    bind_credentials: bool,
    /// Refuse every tool call with 401
    refuse_tool_calls: bool,
    /// Answer `initialize` over SSE with its JSON split across `data:` lines and CRLF endings
    multiline_initialize: bool,
    /// Answer tool calls with an event stream that stays open after the reply
    open_tool_streams: bool,
    sessions: HashSet<String>,
    opened_with: std::collections::HashMap<String, Option<String>>,
    issued: usize,
    seen: Vec<Seen>,
    deleted: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake {
    server: Arc<Mutex<Server>>,
    deleted: Arc<Notify>,
}

impl Fake {
    fn stateless() -> Self {
        let fake = Self::default();
        fake.server.lock().stateless = true;
        fake
    }

    fn service(&self) -> crate::services::http::BoxCloneService {
        let fake = self.clone();
        tower::service_fn(move |request: HttpRequest| {
            let fake = fake.clone();
            async move { fake.handle(request).await }
        })
        .boxed_clone()
    }

    fn initializes(&self) -> usize {
        self.seen_rpc("initialize").len()
    }

    fn seen_rpc(&self, method: &str) -> Vec<Seen> {
        self.server
            .lock()
            .seen
            .iter()
            .filter(|seen| seen.rpc_method.as_deref() == Some(method))
            .cloned()
            .collect()
    }

    fn expire_all(&self) {
        self.server.lock().sessions.clear();
    }

    async fn handle(&self, request: HttpRequest) -> Result<HttpResponse, BoxError> {
        let (parts, body) = request.http_request.into_parts();
        let bytes = body::into_bytes(body).await?;
        let message: Option<Value> = serde_json::from_slice(&bytes).ok();
        let header = |name: &http::HeaderName| {
            parts
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let session = header(&SESSION_ID);
        let seen = Seen {
            method: parts.method.clone(),
            rpc_method: message
                .as_ref()
                .and_then(|message| message.get("method"))
                .and_then(Value::as_str)
                .map(str::to_string),
            rpc_id: message
                .as_ref()
                .and_then(|message| message.get("id"))
                .cloned(),
            session: session.clone(),
            version: header(&PROTOCOL_VERSION),
            authorization: header(&AUTHORIZATION),
            marked: parts.extensions.get::<Marker>().is_some(),
        };
        let mut server = self.server.lock();
        server.seen.push(seen.clone());

        if parts.method == Method::DELETE {
            if let Some(session) = session {
                server.sessions.remove(&session);
                server.deleted.push(session);
            }
            self.deleted.notify_one();
            return Ok(response(StatusCode::OK, None, ""));
        }
        match seen.rpc_method.as_deref() {
            Some("initialize") => {
                if server.refuse_initialize {
                    return Ok(response(StatusCode::UNAUTHORIZED, None, "invalid_token"));
                }
                let result = json!({
                    "jsonrpc": "2.0",
                    "id": seen.rpc_id,
                    "result": { "protocolVersion": "2025-06-18", "capabilities": {} },
                })
                .to_string();
                if server.stateless {
                    return Ok(response(StatusCode::OK, None, &result));
                }
                server.issued += 1;
                let id = format!("session-{}", server.issued);
                server.sessions.insert(id.clone());
                server
                    .opened_with
                    .insert(id.clone(), seen.authorization.clone());
                if server.multiline_initialize {
                    let reply = json!({
                        "jsonrpc": "2.0",
                        "id": seen.rpc_id,
                        "result": { "protocolVersion": "2025-06-18", "capabilities": {} },
                    });
                    let pretty = serde_json::to_string_pretty(&reply).unwrap();
                    let data: String = pretty
                        .lines()
                        .map(|line| format!("data: {line}\r\n"))
                        .collect();
                    return Ok(HttpResponse {
                        http_response: http::Response::builder()
                            .status(StatusCode::OK)
                            .header(SESSION_ID, id)
                            .header(http::header::CONTENT_TYPE, "text/event-stream")
                            .body(body::from_bytes(format!(": ping\r\n\r\n{data}\r\n")))
                            .unwrap(),
                        context: Context::new(),
                    });
                }
                if !server.event_stream {
                    return Ok(response(StatusCode::OK, Some(&id), &result));
                }
                // The reply, then a stream that never ends: reading to the end would hang.
                let reply = Bytes::from(format!("event: message\ndata: {result}\n\n"));
                let stream = futures::stream::once(async move { Ok::<_, BoxError>(reply) })
                    .chain(futures::stream::pending());
                Ok(HttpResponse {
                    http_response: http::Response::builder()
                        .status(StatusCode::OK)
                        .header(SESSION_ID, id)
                        .header(http::header::CONTENT_TYPE, "text/event-stream")
                        .body(body::from_result_stream(stream))
                        .unwrap(),
                    context: Context::new(),
                })
            }
            // A stateless server built to the 2025 lifecycle still expects `initialized`, and the
            // negotiated version on every request after it
            Some("notifications/initialized") if server.stateless => {
                Ok(response(StatusCode::ACCEPTED, None, ""))
            }
            _ if server.stateless => {
                let initialized = server
                    .seen
                    .iter()
                    .any(|seen| seen.rpc_method.as_deref() == Some("notifications/initialized"));
                if !initialized || seen.version.as_deref() != Some("2025-06-18") {
                    return Ok(response(
                        StatusCode::BAD_REQUEST,
                        None,
                        "Bad Request: not initialized, or missing MCP-Protocol-Version",
                    ));
                }
                Ok(response(StatusCode::OK, None, "{}"))
            }
            _ => match session {
                None => Ok(response(
                    StatusCode::BAD_REQUEST,
                    None,
                    "Bad Request: Mcp-Session-Id header is required",
                )),
                Some(session) if !server.sessions.contains(&session) => {
                    Ok(response(StatusCode::NOT_FOUND, None, "Session not found"))
                }
                Some(_) if seen.rpc_id.is_none() => Ok(response(StatusCode::ACCEPTED, None, "")),
                Some(_) if server.refuse_tool_calls => {
                    Ok(response(StatusCode::UNAUTHORIZED, None, "invalid_token"))
                }
                Some(session)
                    if server.bind_credentials
                        && server.opened_with.get(&session) != Some(&seen.authorization) =>
                {
                    Ok(response(
                        StatusCode::UNAUTHORIZED,
                        None,
                        "token does not match session",
                    ))
                }
                Some(_) if server.open_tool_streams => {
                    let reply = json!({ "jsonrpc": "2.0", "id": seen.rpc_id, "result": {} });
                    let event = Bytes::from(format!("data: {reply}\n\n"));
                    let stream = futures::stream::once(async move { Ok::<_, BoxError>(event) })
                        .chain(futures::stream::pending());
                    Ok(HttpResponse {
                        http_response: http::Response::builder()
                            .status(StatusCode::OK)
                            .header(http::header::CONTENT_TYPE, "text/event-stream")
                            .body(body::from_result_stream(stream))
                            .unwrap(),
                        context: Context::new(),
                    })
                }
                Some(_) => Ok(response(
                    StatusCode::OK,
                    None,
                    &json!({ "jsonrpc": "2.0", "id": seen.rpc_id, "result": {} }).to_string(),
                )),
            },
        }
    }
}

fn response(status: StatusCode, session: Option<&str>, body: &str) -> HttpResponse {
    let mut builder = http::Response::builder().status(status);
    if let Some(session) = session {
        builder = builder.header(SESSION_ID, session);
    }
    HttpResponse {
        http_response: builder.body(body::from_bytes(body.to_string())).unwrap(),
        context: Context::new(),
    }
}

fn layer(config: Value) -> McpSessionLayer {
    let config: McpSourceConfiguration = serde_json::from_value(config).unwrap();
    McpSessionLayer::new("mcp.server", &config)
}

fn tool_call(authorization: Option<&str>, context: Context) -> HttpRequest {
    let mut builder = http::Request::builder()
        .method(Method::POST)
        .uri("https://mcp.example.com/mcp");
    if let Some(authorization) = authorization {
        builder = builder.header(AUTHORIZATION, authorization);
    }
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "search", "arguments": {} },
    });
    HttpRequest {
        http_request: builder.body(body::from_bytes(body.to_string())).unwrap(),
        context,
    }
}

fn with_claims(claims: Value) -> Context {
    let context = Context::new();
    context
        .insert(APOLLO_AUTHENTICATION_JWT_CLAIMS, claims)
        .unwrap();
    context
}

#[tokio::test]
async fn initializes_once_and_reuses_the_session() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    for _ in 0..2 {
        let response = service
            .clone()
            .oneshot(tool_call(Some("Bearer a"), Context::new()))
            .await
            .unwrap();
        assert_eq!(response.http_response.status(), StatusCode::OK);
    }

    assert_eq!(fake.initializes(), 1);
    assert_eq!(fake.seen_rpc("notifications/initialized").len(), 1);
    let calls = fake.seen_rpc("tools/call");
    assert_eq!(calls.len(), 2);
    for call in &calls {
        assert_eq!(call.session.as_deref(), Some("session-1"));
        assert_eq!(call.version.as_deref(), Some("2025-06-18"));
        assert_eq!(call.authorization.as_deref(), Some("Bearer a"));
    }
    // Both connector bodies said `"id": 1`; within the session they must differ.
    assert_eq!(calls[0].rpc_id, Some(json!(1)));
    assert_eq!(calls[1].rpc_id, Some(json!(2)));
}

#[tokio::test]
async fn concurrent_requests_wait_on_one_handshake() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    let calls = (0..10).map(|_| {
        service
            .clone()
            .oneshot(tool_call(Some("Bearer a"), Context::new()))
    });
    for response in futures::future::join_all(calls).await {
        assert_eq!(response.unwrap().http_response.status(), StatusCode::OK);
    }

    assert_eq!(fake.initializes(), 1);
    assert_eq!(fake.seen_rpc("tools/call").len(), 10);
}

#[tokio::test]
async fn reads_the_initialize_reply_from_an_open_event_stream() {
    let fake = Fake::default();
    fake.server.lock().event_stream = true;
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        service.oneshot(tool_call(None, Context::new())),
    )
    .await
    .expect("the handshake stops reading at the reply")
    .unwrap();

    assert_eq!(response.http_response.status(), StatusCode::OK);
    assert_eq!(
        fake.seen_rpc("tools/call")[0].session.as_deref(),
        Some("session-1")
    );
}

#[tokio::test]
async fn a_server_that_issues_no_session_still_gets_the_lifecycle() {
    let fake = Fake::stateless();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    for _ in 0..3 {
        let response = service
            .clone()
            .oneshot(tool_call(Some("Bearer a"), Context::new()))
            .await
            .unwrap();
        assert_eq!(response.http_response.status(), StatusCode::OK);
    }

    assert_eq!(fake.initializes(), 1);
    let initialized = fake.seen_rpc("notifications/initialized");
    assert_eq!(
        initialized.len(),
        1,
        "sent even though no session was issued"
    );
    assert_eq!(initialized[0].version.as_deref(), Some("2025-06-18"));
    for call in fake.seen_rpc("tools/call") {
        assert_eq!(call.session, None);
        assert_eq!(call.version.as_deref(), Some("2025-06-18"));
        assert_eq!(call.rpc_id, Some(json!(1)), "the body is sent as written");
    }
}

#[tokio::test]
async fn a_forgotten_session_is_replaced_and_the_request_sent_again() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());
    service
        .clone()
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();

    fake.expire_all();
    let response = service
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();

    assert_eq!(response.http_response.status(), StatusCode::OK);
    assert_eq!(fake.initializes(), 2);
    let sessions: Vec<_> = fake
        .seen_rpc("tools/call")
        .into_iter()
        .map(|call| call.session)
        .collect();
    assert_eq!(
        sessions,
        [
            Some("session-1".to_string()),
            Some("session-1".to_string()),
            Some("session-2".to_string())
        ]
    );
}

#[tokio::test]
async fn callers_with_different_credentials_never_share_a_session() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    for authorization in ["Bearer a", "Bearer b", "Bearer a"] {
        service
            .clone()
            .oneshot(tool_call(Some(authorization), Context::new()))
            .await
            .unwrap();
    }

    let sessions: Vec<_> = fake
        .seen_rpc("tools/call")
        .into_iter()
        .map(|call| (call.authorization.unwrap(), call.session.unwrap()))
        .collect();
    assert_eq!(
        sessions,
        [
            ("Bearer a".to_string(), "session-1".to_string()),
            ("Bearer b".to_string(), "session-2".to_string()),
            ("Bearer a".to_string(), "session-1".to_string()),
        ]
    );
}

#[tokio::test]
async fn a_login_session_keeps_its_mcp_session_across_token_refreshes() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());
    let claims = |sid: &str| json!({ "iss": "https://idp", "sub": "ada", "sid": sid });

    for (token, sid) in [
        ("Bearer t1", "login-1"),
        ("Bearer t2", "login-1"),
        ("Bearer t3", "login-2"),
    ] {
        service
            .clone()
            .oneshot(tool_call(Some(token), with_claims(claims(sid))))
            .await
            .unwrap();
    }

    let sessions: Vec<_> = fake
        .seen_rpc("tools/call")
        .into_iter()
        .map(|call| call.session.unwrap())
        .collect();
    assert_eq!(sessions, ["session-1", "session-1", "session-2"]);
}

#[tokio::test]
async fn a_refused_initialize_is_the_connectors_response_and_is_retried_next_time() {
    let fake = Fake::default();
    fake.server.lock().refuse_initialize = true;
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    let response = service
        .clone()
        .oneshot(tool_call(Some("Bearer expired"), Context::new()))
        .await
        .unwrap();
    assert_eq!(response.http_response.status(), StatusCode::UNAUTHORIZED);
    assert!(fake.seen_rpc("tools/call").is_empty());

    fake.server.lock().refuse_initialize = false;
    let response = service
        .oneshot(tool_call(Some("Bearer expired"), Context::new()))
        .await
        .unwrap();
    assert_eq!(response.http_response.status(), StatusCode::OK);
    assert_eq!(fake.initializes(), 2);
}

#[tokio::test]
async fn a_session_evicted_to_make_room_is_closed() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto", "max_sessions": 1 })).layer(fake.service());

    let deleted = fake.deleted.notified();
    for authorization in ["Bearer a", "Bearer b"] {
        service
            .clone()
            .oneshot(tool_call(Some(authorization), Context::new()))
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), deleted)
        .await
        .expect("the evicted session is closed");

    let server = fake.server.lock();
    assert_eq!(server.deleted, ["session-1"]);
    let delete = server
        .seen
        .iter()
        .find(|seen| seen.method == Method::DELETE)
        .unwrap();
    assert_eq!(
        delete.authorization.as_deref(),
        Some("Bearer a"),
        "closed with the credentials that opened it"
    );
}

#[tokio::test]
async fn requests_that_are_not_json_rpc_pass_through() {
    let fake = Fake::default();
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());
    let request = HttpRequest {
        http_request: http::Request::builder()
            .method(Method::POST)
            .uri("https://mcp.example.com/upload")
            .body(body::from_bytes("not json"))
            .unwrap(),
        context: Context::new(),
    };

    service.oneshot(request).await.unwrap();

    assert_eq!(fake.initializes(), 0);
}

#[tokio::test]
async fn a_session_tied_to_a_refreshed_token_is_replaced_and_the_request_sent_again() {
    async {
        let fake = Fake::default();
        fake.server.lock().bind_credentials = true;
        let service = layer(json!({ "sessions": "auto" })).layer(fake.service());
        let claims = json!({ "iss": "https://idp", "sub": "ada", "sid": "login-1" });

        let deleted = fake.deleted.notified();
        for token in ["Bearer t1", "Bearer t2"] {
            let response = service
                .clone()
                .oneshot(tool_call(Some(token), with_claims(claims.clone())))
                .await
                .unwrap();
            assert_eq!(response.http_response.status(), StatusCode::OK, "{token}");
        }
        tokio::time::timeout(Duration::from_secs(5), deleted)
            .await
            .expect("the session opened with the old token is closed");

        let calls: Vec<_> = fake
            .seen_rpc("tools/call")
            .into_iter()
            .map(|call| (call.authorization.unwrap(), call.session.unwrap()))
            .collect();
        assert_eq!(
            calls,
            [
                ("Bearer t1".to_string(), "session-1".to_string()),
                // Refused: the server ties session-1 to t1
                ("Bearer t2".to_string(), "session-1".to_string()),
                ("Bearer t2".to_string(), "session-2".to_string()),
            ]
        );
        let server = fake.server.lock();
        assert_eq!(server.deleted, ["session-1"]);
        let delete = server
            .seen
            .iter()
            .find(|seen| seen.method == Method::DELETE)
            .unwrap();
        assert_eq!(delete.authorization.as_deref(), Some("Bearer t1"));
        drop(server);

        assert_counter!(
            "apollo.router.connectors.mcp.handshakes",
            1,
            "connector.source" = "mcp.server",
            "mcp.handshake.reason" = "new",
            "mcp.handshake.outcome" = "session"
        );
        assert_counter!(
            "apollo.router.connectors.mcp.handshakes",
            1,
            "connector.source" = "mcp.server",
            "mcp.handshake.reason" = "credentials_changed",
            "mcp.handshake.outcome" = "session"
        );
    }
    .with_metrics()
    .await;
}

#[tokio::test]
async fn a_request_refused_with_the_credentials_that_opened_its_session_is_not_retried() {
    let fake = Fake::default();
    fake.server.lock().refuse_tool_calls = true;
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    let response = service
        .oneshot(tool_call(Some("Bearer revoked"), Context::new()))
        .await
        .unwrap();

    assert_eq!(response.http_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fake.initializes(), 1);
    assert_eq!(fake.seen_rpc("tools/call").len(), 1);
}

#[tokio::test]
async fn open_sessions_are_closed_when_the_layer_is_dropped() {
    let fake = Fake::default();
    let layer = layer(json!({ "sessions": "auto" }));
    let service = layer.layer(fake.service());

    for authorization in ["Bearer a", "Bearer b"] {
        service
            .clone()
            .oneshot(tool_call(Some(authorization), Context::new()))
            .await
            .unwrap();
    }
    assert!(fake.server.lock().deleted.is_empty());

    // A reload drops the old pipeline's layer and every service built from it
    drop(service);
    drop(layer);
    tokio::time::timeout(Duration::from_secs(5), async {
        while fake.server.lock().deleted.len() < 2 {
            fake.deleted.notified().await;
        }
    })
    .await
    .expect("both sessions are closed");

    let mut deleted = fake.server.lock().deleted.clone();
    deleted.sort();
    assert_eq!(deleted, ["session-1", "session-2"]);
}

#[tokio::test]
async fn every_request_the_layer_sends_keeps_the_connector_requests_extensions() {
    let fake = Fake::default();
    let layer = layer(json!({ "sessions": "auto" }));
    let service = layer.layer(fake.service());

    let mut request = tool_call(Some("Bearer a"), Context::new());
    request.http_request.extensions_mut().insert(Marker);
    service.oneshot(request).await.unwrap();

    let deleted = fake.deleted.notified();
    drop(layer);
    tokio::time::timeout(Duration::from_secs(5), deleted)
        .await
        .expect("the session is closed");

    let seen = fake.server.lock().seen.clone();
    let kinds: Vec<_> = seen
        .iter()
        .map(|seen| {
            (
                seen.rpc_method
                    .clone()
                    .unwrap_or_else(|| seen.method.to_string()),
                seen.marked,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("initialize".to_string(), true),
            ("notifications/initialized".to_string(), true),
            ("tools/call".to_string(), true),
            ("DELETE".to_string(), true),
        ],
        "SigV4 signing parameters ride in extensions, so every request must keep them"
    );
}

#[tokio::test]
async fn an_initialize_reply_split_across_data_lines_is_read() {
    let fake = Fake::default();
    fake.server.lock().multiline_initialize = true;
    let service = layer(json!({ "sessions": "auto" })).layer(fake.service());

    let response = service
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();

    assert_eq!(response.http_response.status(), StatusCode::OK);
    let calls = fake.seen_rpc("tools/call");
    assert_eq!(calls[0].session.as_deref(), Some("session-1"));
}

#[tokio::test]
async fn an_evicted_session_stays_open_until_its_response_body_is_done() {
    let fake = Fake::default();
    fake.server.lock().open_tool_streams = true;
    let service = layer(json!({ "sessions": "auto", "max_sessions": 1 })).layer(fake.service());

    // A's tool call answers with an event stream that is still open
    let a_response = service
        .clone()
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();

    // B's session takes the only place in the cache, evicting A's slot
    fake.server.lock().open_tool_streams = false;
    service
        .clone()
        .oneshot(tool_call(Some("Bearer b"), Context::new()))
        .await
        .unwrap();
    // Let any DELETE spawned so far run before checking that there was none
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        fake.server.lock().deleted.is_empty(),
        "A's session is still in use while its body is unread"
    );

    let deleted = fake.deleted.notified();
    drop(a_response);
    tokio::time::timeout(Duration::from_secs(5), deleted)
        .await
        .expect("A's session closes once its body is dropped");
    assert_eq!(fake.server.lock().deleted, ["session-1"]);
}

#[tokio::test]
async fn an_expired_session_is_not_deleted() {
    let fake = Fake::default();
    let layer = layer(json!({ "sessions": "auto" }));
    let service = layer.layer(fake.service());

    service
        .clone()
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();
    fake.expire_all();
    service
        .clone()
        .oneshot(tool_call(Some("Bearer a"), Context::new()))
        .await
        .unwrap();

    // Closing the layer closes only the session the server still knows
    let deleted = fake.deleted.notified();
    drop(service);
    drop(layer);
    tokio::time::timeout(Duration::from_secs(5), deleted)
        .await
        .expect("the live session is closed");
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(fake.server.lock().deleted, ["session-2"]);
}

#[tokio::test]
async fn session_adoption_is_counted_by_what_switched_it_on() {
    async {
        let configuration: crate::Configuration = serde_json::from_value(json!({
            "connectors": {
                "sources": {
                    "a.by_config": { "mcp": { "sessions": "auto" } },
                    "b.off": { "mcp": { "sessions": "off" } }
                }
            }
        }))
        .unwrap();
        let mcp_sources: HashSet<String> = ["c.by_schema".to_string(), "b.off".to_string()].into();

        let layers = super::layers(&configuration, &mcp_sources);

        let mut sources: Vec<_> = layers.keys().cloned().collect();
        sources.sort();
        assert_eq!(sources, ["a.by_config", "c.by_schema"]);
        assert_gauge!(
            "apollo.router.connectors.mcp.sessions",
            1,
            "mcp.sessions.enabled_by" = "schema"
        );
        assert_gauge!(
            "apollo.router.connectors.mcp.sessions",
            1,
            "mcp.sessions.enabled_by" = "config"
        );
    }
    .with_metrics()
    .await;
}
