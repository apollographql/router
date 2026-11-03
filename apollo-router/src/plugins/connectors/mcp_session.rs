//! MCP sessions for connector sources.
//!
//! An MCP server built to a revision before 2026-07-28 may require a client to `initialize`
//! first and then send the `Mcp-Session-Id` it was issued on every request. A connector sends
//! one JSON-RPC request per fetch and knows nothing of that, so this layer does the handshake for
//! it: once per caller, reusing the session across operations, and again when the server forgets
//! it. A server that issues no session id is remembered as stateless and left alone.
//!
//! The layer sits innermost in a source's HTTP client stack, directly above
//! [`HttpClientService`](crate::services::http::HttpClientService), so it sees the request exactly
//! as it goes on the wire, after every plugin has had its turn.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context as TaskContext;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use bytes::Bytes;
use bytes::BytesMut;
use futures::future::BoxFuture;
use hmac::Hmac;
use hmac::KeyInit;
use hmac::Mac;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::AUTHORIZATION;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::header::COOKIE;
use http::header::PROXY_AUTHORIZATION;
use http::request::Parts;
use http_body_util::BodyExt;
use lru::LruCache;
use parking_lot::Mutex;
use serde_json::Value;
use serde_json::json;
use tokio::sync::OnceCell;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceExt;

use crate::Configuration;
use crate::Context;
use crate::plugins::authentication::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use crate::plugins::connectors::configuration::ConnectorsConfig;
use crate::plugins::connectors::configuration::McpSessionsMode;
use crate::plugins::connectors::configuration::McpSourceConfiguration;
use crate::plugins::connectors::plugin::PLUGIN_NAME;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router::body;
use crate::services::router::body::RouterBody;

type HmacSha256 = Hmac<sha2::Sha256>;

const SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");
const PROTOCOL_VERSION: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// The revision asked for in `initialize`: the most widely served one that still has sessions.
const REQUESTED_VERSION: &str = "2025-06-18";
/// The first revision without sessions. A server that negotiates it, or a later one, is stateless.
const STATELESS_SINCE: &str = "2026-07-28";
/// How much of an `initialize` response is read while looking for the JSON-RPC result.
const MAX_HANDSHAKE_BODY: usize = 1 << 20;

/// Headers that identify the caller whenever the request carries no validated JWT.
const DEFAULT_IDENTITY_HEADERS: [HeaderName; 3] = [AUTHORIZATION, COOKIE, PROXY_AUTHORIZATION];

/// A session layer for each connector source configured with `mcp.sessions: auto`, keyed like
/// the connector HTTP clients (`{subgraph_name}.{source_name}`).
pub(crate) fn layers(configuration: &Configuration) -> HashMap<String, McpSessionLayer> {
    let Some(config) =
        configuration.typed_plugin_config::<ConnectorsConfig>(&format!("apollo.{PLUGIN_NAME}"))
    else {
        return HashMap::new();
    };
    config
        .sources
        .iter()
        .filter_map(|(source, source_config)| {
            let mcp = source_config.mcp.as_ref()?;
            (mcp.sessions == McpSessionsMode::Auto)
                .then(|| (source.clone(), McpSessionLayer::new(source, mcp)))
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct McpSessionLayer {
    sessions: Arc<Sessions>,
}

impl McpSessionLayer {
    pub(crate) fn new(source: &str, config: &McpSourceConfiguration) -> Self {
        let capacity = NonZeroUsize::new(config.max_sessions).unwrap_or(NonZeroUsize::MIN);
        Self {
            sessions: Arc::new(Sessions {
                source: source.to_string(),
                identity_headers: config
                    .identity_headers
                    .iter()
                    .filter_map(|name| HeaderName::try_from(name.as_str()).ok())
                    .collect(),
                idle_timeout: *config.idle_timeout,
                hmac_key: rand::random(),
                slots: Mutex::new(LruCache::new(capacity)),
            }),
        }
    }
}

impl<S> Layer<S> for McpSessionLayer {
    type Service = McpSessionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        McpSessionService {
            inner,
            sessions: self.sessions.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct McpSessionService<S> {
    inner: S,
    sessions: Arc<Sessions>,
}

impl<S> Service<HttpRequest> for McpSessionService<S>
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<HttpResponse, BoxError>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        // Keep the service that was polled ready for this call, leaving a fresh clone behind.
        let clone = self.inner.clone();
        let inner = std::mem::replace(&mut self.inner, clone);
        let sessions = self.sessions.clone();
        Box::pin(async move { sessions.call(inner, request).await })
    }
}

/// The sessions of one source, keyed by caller.
struct Sessions {
    source: String,
    identity_headers: Vec<HeaderName>,
    idle_timeout: Duration,
    /// Random per process, so the keys say nothing about the credentials they were derived from.
    hmac_key: [u8; 32],
    slots: Mutex<LruCache<Key, Arc<Slot>>>,
}

type Key = [u8; 32];

/// What is known about one caller's session. Requests that arrive together wait on one
/// handshake; a handshake that fails leaves the cell empty, so the next request tries again.
#[derive(Default)]
struct Slot {
    state: OnceCell<State>,
}

enum State {
    /// The server issued no session id, or negotiated a revision without sessions.
    Stateless,
    Session(Box<Session>),
}

struct Session {
    id: HeaderValue,
    version: HeaderValue,
    /// JSON-RPC ids must be unique within a session, and connector bodies usually all say `1`.
    next_request_id: AtomicU64,
    last_used: Mutex<Instant>,
    /// The request that opened the session, without its body, so the session can be closed with
    /// the same URI and credentials.
    opened_by: Parts,
}

enum HandshakeError {
    /// The server answered `initialize` or `notifications/initialized` with something other
    /// than success. That answer is passed back to the connector as the response to its request.
    Refused(Box<HttpResponse>),
    Failed(BoxError),
}

impl From<BoxError> for HandshakeError {
    fn from(error: BoxError) -> Self {
        HandshakeError::Failed(error)
    }
}

impl Sessions {
    async fn call<S>(
        self: Arc<Self>,
        inner: S,
        request: HttpRequest,
    ) -> Result<HttpResponse, BoxError>
    where
        S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
        S::Future: Send,
    {
        let HttpRequest {
            http_request,
            context,
        } = request;
        if http_request.method() != Method::POST {
            return inner
                .oneshot(HttpRequest {
                    http_request,
                    context,
                })
                .await;
        }
        let (parts, body) = http_request.into_parts();
        let bytes = body::into_bytes(body).await?;
        let Some(mut message) = session_message(&bytes) else {
            // Not JSON-RPC, or an `initialize` the connector wrote itself: send it as it is.
            return inner.oneshot(rebuild(&parts, bytes, &context)).await;
        };

        let key = self.key(&parts.headers, &context);
        for attempt in 0..2 {
            let slot = self.slot(key, inner.clone());
            // Move a clone of the service into the handshake rather than borrowing it, so the
            // future needs only `Send`, not `Sync`.
            let (handshake, this, parts_ref, context_ref) =
                (inner.clone(), &*self, &parts, &context);
            let state = match slot
                .state
                .get_or_try_init(move || this.initialize(handshake, parts_ref, context_ref))
                .await
            {
                Ok(state) => state,
                Err(HandshakeError::Refused(response)) => return Ok(*response),
                Err(HandshakeError::Failed(error)) => return Err(error),
            };
            let State::Session(session) = state else {
                return inner.oneshot(rebuild(&parts, bytes, &context)).await;
            };

            let request = session.request(&parts, &mut message, &context)?;
            let response = inner.clone().oneshot(request).await?;
            if response.http_response.status() == StatusCode::NOT_FOUND && attempt == 0 {
                // The server no longer knows the session. It did not process the request, so
                // starting a new session and sending it again is safe.
                tracing::debug!(source = %self.source, "MCP session expired; initializing again");
                self.forget(&key, &slot);
                continue;
            }
            *session.last_used.lock() = Instant::now();
            return Ok(response);
        }
        unreachable!("the second attempt always returns")
    }

    /// One key per caller: the validated JWT's issuer, subject and login session when there is
    /// one, otherwise the identity headers. Configured identity headers count in both cases, so
    /// a client can ask for separate sessions (one per conversation, say).
    fn key(&self, headers: &HeaderMap, context: &Context) -> Key {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC takes keys of any size");
        let claims = context.get_json_value(APOLLO_AUTHENTICATION_JWT_CLAIMS);
        let subject = claims
            .as_ref()
            .and_then(|claims| claims.get("sub"))
            .and_then(|sub| sub.as_str());
        if let (Some(claims), Some(_)) = (&claims, subject) {
            mac.update(b"jwt\0");
            for claim in ["iss", "sub", "sid"] {
                let value = claims
                    .get(claim)
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                mac.update(value.as_bytes());
                mac.update(b"\0");
            }
        } else {
            mac.update(b"headers\0");
            hash_headers(&mut mac, headers, &DEFAULT_IDENTITY_HEADERS);
        }
        hash_headers(&mut mac, headers, &self.identity_headers);
        mac.finalize().into_bytes().into()
    }

    /// The slot for `key`, replacing one whose session has been idle too long. Sessions that
    /// are dropped here, for idling or to make room, are closed in the background.
    fn slot<S>(&self, key: Key, inner: S) -> Arc<Slot>
    where
        S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
        S::Future: Send,
    {
        let mut closing = Vec::new();
        let slot = {
            let mut slots = self.slots.lock();
            if let Some(slot) = slots.get(&key) {
                match slot.state.get() {
                    Some(State::Session(session))
                        if session.last_used.lock().elapsed() > self.idle_timeout =>
                    {
                        if let Some(slot) = slots.pop(&key) {
                            closing.push(slot);
                        }
                    }
                    _ => return slot.clone(),
                }
            }
            let slot = Arc::new(Slot::default());
            if let Some((_, evicted)) = slots.push(key, slot.clone()) {
                closing.push(evicted);
            }
            slot
        };
        for slot in closing {
            close(slot, inner.clone());
        }
        slot
    }

    /// Drops `slot` from the cache, unless another request has already replaced it.
    fn forget(&self, key: &Key, slot: &Arc<Slot>) {
        let mut slots = self.slots.lock();
        if slots
            .peek(key)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            slots.pop(key);
        }
    }

    async fn initialize<S>(
        &self,
        inner: S,
        parts: &Parts,
        context: &Context,
    ) -> Result<State, HandshakeError>
    where
        S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
        S::Future: Send,
    {
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": REQUESTED_VERSION,
                // None: the router cannot answer sampling, elicitation or roots requests.
                "capabilities": {},
                "clientInfo": { "name": "apollo-router", "version": env!("CARGO_PKG_VERSION") },
            },
        });
        let request = handshake_request(parts, &initialize, None, context);
        let response = inner.clone().oneshot(request).await?;
        let HttpResponse {
            http_response,
            context: response_context,
        } = response;
        let (response_parts, response_body) = http_response.into_parts();
        let session_id = response_parts.headers.get(&SESSION_ID).cloned();
        let event_stream = response_parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let (read, reply) = first_reply(response_body, event_stream).await?;
        let refused = !response_parts.status.is_success()
            || reply
                .as_ref()
                .is_none_or(|reply| reply.get("result").is_none());
        if refused {
            return Err(HandshakeError::Refused(Box::new(HttpResponse {
                http_response: http::Response::from_parts(response_parts, body::from_bytes(read)),
                context: response_context,
            })));
        }
        let version = reply
            .as_ref()
            .and_then(|reply| reply.pointer("/result/protocolVersion"))
            .and_then(|version| version.as_str())
            .unwrap_or(REQUESTED_VERSION)
            .to_string();
        let Some(session_id) = session_id else {
            tracing::debug!(source = %self.source, "MCP server issued no session; treating it as stateless");
            return Ok(State::Stateless);
        };
        if version.as_str() >= STATELESS_SINCE {
            return Ok(State::Stateless);
        }
        let version = HeaderValue::try_from(version).map_err(BoxError::from)?;

        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let request =
            handshake_request(parts, &initialized, Some((&session_id, &version)), context);
        let response = inner.oneshot(request).await?;
        if !response.http_response.status().is_success() {
            return Err(HandshakeError::Refused(Box::new(response)));
        }
        tracing::debug!(source = %self.source, "MCP session initialized");

        let mut opened_by = parts.clone();
        opened_by.headers.remove(CONTENT_LENGTH);
        Ok(State::Session(Box::new(Session {
            id: session_id,
            version,
            next_request_id: AtomicU64::new(1),
            last_used: Mutex::new(Instant::now()),
            opened_by,
        })))
    }
}

impl Session {
    /// The connector's request, sent within this session.
    fn request(
        &self,
        parts: &Parts,
        message: &mut Value,
        context: &Context,
    ) -> Result<HttpRequest, BoxError> {
        if let Some(id) = message.get_mut("id") {
            *id = self.next_request_id.fetch_add(1, Ordering::Relaxed).into();
        }
        let mut parts = parts.clone();
        parts.headers.remove(CONTENT_LENGTH);
        parts.headers.insert(SESSION_ID, self.id.clone());
        parts.headers.insert(PROTOCOL_VERSION, self.version.clone());
        Ok(HttpRequest {
            http_request: http::Request::from_parts(
                parts,
                body::from_bytes(serde_json::to_vec(message)?),
            ),
            context: context.clone(),
        })
    }
}

/// The JSON-RPC request or notification in `bytes`, unless it is not one, or is an
/// `initialize`, which must not be sent within a session.
fn session_message(bytes: &Bytes) -> Option<Value> {
    let message: Value = serde_json::from_slice(bytes).ok()?;
    let method = message.get("method")?.as_str()?;
    (method != "initialize").then_some(message)
}

fn hash_headers(mac: &mut HmacSha256, headers: &HeaderMap, names: &[HeaderName]) {
    for name in names {
        for value in headers.get_all(name) {
            mac.update(name.as_str().as_bytes());
            mac.update(b"\0");
            mac.update(value.as_bytes());
            mac.update(b"\0");
        }
    }
}

fn rebuild(parts: &Parts, bytes: Bytes, context: &Context) -> HttpRequest {
    HttpRequest {
        http_request: http::Request::from_parts(parts.clone(), body::from_bytes(bytes)),
        context: context.clone(),
    }
}

/// A handshake message sent to the connector's URI with the connector's headers (and so its
/// credentials), within `session` when there is one.
fn handshake_request(
    parts: &Parts,
    message: &Value,
    session: Option<(&HeaderValue, &HeaderValue)>,
    context: &Context,
) -> HttpRequest {
    let mut parts = parts.clone();
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.remove(&SESSION_ID);
    parts.headers.remove(&PROTOCOL_VERSION);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    parts.headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some((id, version)) = session {
        parts.headers.insert(SESSION_ID, id.clone());
        parts.headers.insert(PROTOCOL_VERSION, version.clone());
    }
    HttpRequest {
        http_request: http::Request::from_parts(parts, body::from_bytes(message.to_string())),
        context: context.clone(),
    }
}

/// Reads `body` until it holds a JSON-RPC response, returning what was read and that response.
/// An event stream can stay open after the response, so it is not read to the end.
async fn first_reply(
    mut body: RouterBody,
    event_stream: bool,
) -> Result<(Bytes, Option<Value>), BoxError> {
    let mut read = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            read.extend_from_slice(&data);
            if read.len() > MAX_HANDSHAKE_BODY {
                return Err("MCP initialize response is too large".into());
            }
            if event_stream && let Some(reply) = event_stream_reply(&read) {
                return Ok((read.freeze(), Some(reply)));
            }
        }
    }
    let reply = if event_stream {
        event_stream_reply(&read)
    } else {
        serde_json::from_slice(&read).ok()
    };
    Ok((read.freeze(), reply))
}

/// The first JSON-RPC response among the complete `data:` lines of an event stream.
fn event_stream_reply(read: &[u8]) -> Option<Value> {
    let complete = &read[..=read.iter().rposition(|&byte| byte == b'\n')?];
    std::str::from_utf8(complete)
        .ok()?
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message.get("result").is_some() || message.get("error").is_some())
}

/// Ends the session held by `slot`, if it holds one, with the credentials that opened it. This
/// is a courtesy that frees the server's resources early; its outcome is ignored.
fn close<S>(slot: Arc<Slot>, inner: S)
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Send + 'static,
    S::Future: Send,
{
    let Some(State::Session(session)) = slot.state.get() else {
        return;
    };
    let mut parts = session.opened_by.clone();
    parts.method = Method::DELETE;
    parts.headers.remove(CONTENT_TYPE);
    parts.headers.insert(SESSION_ID, session.id.clone());
    parts
        .headers
        .insert(PROTOCOL_VERSION, session.version.clone());
    let request = HttpRequest {
        http_request: http::Request::from_parts(parts, body::empty()),
        context: Context::new(),
    };
    tokio::spawn(async move {
        let _ = inner.oneshot(request).await;
    });
}

#[cfg(test)]
mod tests;
