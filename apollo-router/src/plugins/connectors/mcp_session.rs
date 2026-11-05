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
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context as TaskContext;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use apollo_federation::connectors::runtime::stream_framing::StreamFraming;
use bytes::Bytes;
use bytes::BytesMut;
use futures::StreamExt;
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
use http_body_util::BodyDataStream;
use http_body_util::BodyExt;
use lru::LruCache;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;
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
use crate::metrics::meter_provider;
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

/// A session layer for each connector source that holds MCP sessions, keyed like the connector
/// HTTP clients (`{subgraph_name}.{source_name}`).
///
/// `mcp_sources` are the sources whose schema says they are MCP servers, with `@source(mcp:)`.
/// They hold sessions unless their router config sets `mcp.sessions: off`. Any other source
/// holds sessions only if its router config sets `mcp.sessions: auto`, for connectors that write
/// the MCP request themselves with `http:`.
pub(crate) fn layers(
    configuration: &Configuration,
    mcp_sources: &HashSet<String>,
) -> HashMap<String, McpSessionLayer> {
    let config =
        configuration.typed_plugin_config::<ConnectorsConfig>(&format!("apollo.{PLUGIN_NAME}"));
    let configured = config
        .iter()
        .flat_map(|config| config.sources.iter())
        .filter_map(|(source, source_config)| Some((source, source_config.mcp.as_ref()?)));
    let defaults = McpSourceConfiguration::default();
    let unconfigured = mcp_sources
        .iter()
        .filter(|source| {
            !config.as_ref().is_some_and(|config| {
                config
                    .sources
                    .get(*source)
                    .is_some_and(|source_config| source_config.mcp.is_some())
            })
        })
        .map(|source| (source, &defaults));

    let mut by_schema = 0u64;
    let mut by_config = 0u64;
    let layers: HashMap<String, McpSessionLayer> = configured
        .chain(unconfigured)
        .filter_map(|(source, mcp)| {
            let default_mode = if mcp_sources.contains(source) {
                McpSessionsMode::Auto
            } else {
                McpSessionsMode::Off
            };
            match mcp.sessions {
                Some(McpSessionsMode::Auto) => by_config += 1,
                None if default_mode == McpSessionsMode::Auto => by_schema += 1,
                _ => return None,
            }
            Some((source.clone(), McpSessionLayer::new(source, mcp)))
        })
        .collect();

    // Adoption of sessions, whichever way they were switched on, for as long as these layers are
    // in use: set once per pipeline, so it is visible with no traffic at all.
    if !layers.is_empty() {
        let gauge = Arc::new(sessions_instrument(by_schema, by_config));
        for layer in layers.values() {
            let _ = layer.sessions.instrument.set(gauge.clone());
        }
    }
    layers
}

/// `apollo.router.connectors.mcp.sessions`: the number of connector sources holding MCP
/// sessions, by `mcp.sessions.enabled_by`: `schema` for an `@source(mcp:)` source with no session
/// setting, `config` for a source whose router config sets `mcp.sessions: auto`.
fn sessions_instrument(by_schema: u64, by_config: u64) -> ObservableGauge<u64> {
    meter_provider()
        .meter("apollo/router")
        .u64_observable_gauge("apollo.router.connectors.mcp.sessions")
        .with_description("Number of connector sources holding MCP sessions")
        .with_callback(move |observer| {
            for (enabled_by, count) in [("schema", by_schema), ("config", by_config)] {
                if count > 0 {
                    observer.observe(
                        count,
                        &[KeyValue::new("mcp.sessions.enabled_by", enabled_by)],
                    );
                }
            }
        })
        .build()
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
                // The connectors plugin refuses a configuration with an invalid name, so nothing
                // is dropped here
                identity_headers: config
                    .identity_headers
                    .iter()
                    .filter_map(|name| HeaderName::try_from(name.as_str()).ok())
                    .collect(),
                idle_timeout: *config.idle_timeout,
                hmac_key: rand::random(),
                slots: Mutex::new(LruCache::new(capacity)),
                closer: Arc::new(OnceLock::new()),
                instrument: OnceLock::new(),
            }),
        }
    }
}

impl<S> Layer<S> for McpSessionLayer
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
{
    type Service = McpSessionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        // Keep a client for closing the sessions that are still open when the layer goes away,
        // on a reload or at shutdown.
        let closer = Mutex::new(inner.clone());
        let _ = self.sessions.closer.set(Box::new(move |session| {
            let inner = closer.lock().clone();
            close(session, inner);
        }));
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
    /// Closes one session, through the client the layer wraps. Set when the layer is applied, and
    /// shared with every slot, which closes its own session when the last holder lets it go.
    closer: Arc<OnceLock<Closer>>,
    /// The adoption gauge, kept alive with the layers it counts
    instrument: OnceLock<Arc<ObservableGauge<u64>>>,
}

type Closer = Box<dyn Fn(&Session) + Send + Sync>;

/// Why a handshake happens, for the handshake counter.
#[derive(Clone, Copy)]
enum HandshakeReason {
    /// The caller had no session yet.
    New,
    /// The server answered 404: it no longer knows the session.
    Expired,
    /// The server answered 401 or 403 to a session opened with other credentials, as when a
    /// token is refreshed and the server ties sessions to tokens.
    CredentialsChanged,
}

impl HandshakeReason {
    fn as_str(self) -> &'static str {
        match self {
            HandshakeReason::New => "new",
            HandshakeReason::Expired => "expired",
            HandshakeReason::CredentialsChanged => "credentials_changed",
        }
    }
}

type Key = [u8; 32];

/// What is known about one caller's session. Requests that arrive together wait on one
/// handshake; a handshake that fails leaves the cell empty, so the next request tries again.
///
/// A slot closes its session when it is dropped, which happens only after the cache has let go of
/// it (for eviction, idling, a refused credential, or the layer itself going away on a reload)
/// *and* every request using it is done, including reading its response body. So a session is
/// never closed under a request that is still using it.
struct Slot {
    state: OnceCell<State>,
    closer: Arc<OnceLock<Closer>>,
    /// Set when the server has already forgotten the session, so there is nothing to close.
    gone: AtomicBool,
}

impl Slot {
    fn new(closer: Arc<OnceLock<Closer>>) -> Self {
        Self {
            state: OnceCell::new(),
            closer,
            gone: AtomicBool::new(false),
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if self.gone.load(Ordering::Relaxed) {
            return;
        }
        if let (Some(State::Session(session)), Some(closer)) = (self.state.get(), self.closer.get())
        {
            closer(session);
        }
    }
}

enum State {
    /// The server issued no session id. Requests go without one, but with the protocol version
    /// negotiated in `initialize`, which every request after it must carry. `None` when the
    /// server negotiated a revision without the handshake, whose requests are sent as written.
    Stateless {
        version: Option<HeaderValue>,
    },
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
    ///
    /// This keeps the opening request's headers, including `Authorization`, `Cookie` and any
    /// API-key header, in memory for as long as the session lives: closing it needs the
    /// credentials that opened it, which may be in any header a connector sets. They are never
    /// logged or written anywhere, and are dropped with the session.
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
        let mut reason = HandshakeReason::New;
        for attempt in 0..2 {
            let slot = self.slot(key);
            // Move a clone of the service into the handshake rather than borrowing it, so the
            // future needs only `Send`, not `Sync`.
            let (handshake, this, parts_ref, context_ref) =
                (inner.clone(), &*self, &parts, &context);
            let state = match slot
                .state
                .get_or_try_init(move || {
                    this.counted_initialize(handshake, parts_ref, context_ref, reason)
                })
                .await
            {
                Ok(state) => state,
                Err(HandshakeError::Refused(response)) => return Ok(*response),
                Err(HandshakeError::Failed(error)) => return Err(error),
            };
            let session = match state {
                State::Session(session) => session,
                State::Stateless { version } => {
                    let mut request = rebuild(&parts, bytes, &context);
                    if let Some(version) = version {
                        request
                            .http_request
                            .headers_mut()
                            .insert(PROTOCOL_VERSION, version.clone());
                    }
                    return inner.oneshot(request).await;
                }
            };

            let request = session.request(&parts, &mut message, &context)?;
            let response = inner.clone().oneshot(request).await?;
            let status = response.http_response.status();
            if status == StatusCode::NOT_FOUND && attempt == 0 {
                // The server no longer knows the session. It did not process the request, so
                // starting a new session and sending it again is safe.
                tracing::debug!(source = %self.source, "MCP session expired; initializing again");
                slot.gone.store(true, Ordering::Relaxed);
                self.forget(&key, &slot);
                reason = HandshakeReason::Expired;
                continue;
            }
            if (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
                && attempt == 0
                && credentials_changed(&session.opened_by.headers, &parts.headers)
            {
                // The session was opened with other credentials, and this server ties sessions
                // to them: open one with the credentials the request carries now. The request
                // was refused, so sending it again is safe. A request refused with the same
                // credentials that opened the session is not retried.
                tracing::debug!(
                    source = %self.source,
                    "MCP session belongs to earlier credentials; initializing again"
                );
                // Closed with the old credentials once this request lets go of the slot
                self.forget(&key, &slot);
                reason = HandshakeReason::CredentialsChanged;
                continue;
            }
            *session.last_used.lock() = Instant::now();
            // The session stays open while the response body is read, which for an event stream
            // happens after this returns.
            return Ok(hold_while_reading(response, slot.clone()));
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

    /// The slot for `key`, replacing one whose session has been idle too long. A slot dropped
    /// here, for idling or to make room, closes its session once no request is using it.
    fn slot(&self, key: Key) -> Arc<Slot> {
        // Dropped after the lock is released, since dropping a slot may close its session
        let mut released = Vec::new();
        let slot = {
            let mut slots = self.slots.lock();
            if let Some(slot) = slots.get(&key) {
                match slot.state.get() {
                    Some(State::Session(session))
                        if session.last_used.lock().elapsed() > self.idle_timeout =>
                    {
                        released.extend(slots.pop(&key));
                    }
                    _ => return slot.clone(),
                }
            }
            let slot = Arc::new(Slot::new(self.closer.clone()));
            if let Some((_, evicted)) = slots.push(key, slot.clone()) {
                released.push(evicted);
            }
            slot
        };
        drop(released);
        slot
    }

    /// Drops `slot` from the cache, unless another request has already replaced it.
    fn forget(&self, key: &Key, slot: &Arc<Slot>) {
        let released = {
            let mut slots = self.slots.lock();
            if slots
                .peek(key)
                .is_some_and(|current| Arc::ptr_eq(current, slot))
            {
                slots.pop(key)
            } else {
                None
            }
        };
        drop(released);
    }

    /// [`Self::initialize`], counted by why it happened and how it ended. The handshake goes
    /// below the HTTP client's own instruments, so this counter is where it shows up.
    async fn counted_initialize<S>(
        &self,
        inner: S,
        parts: &Parts,
        context: &Context,
        reason: HandshakeReason,
    ) -> Result<State, HandshakeError>
    where
        S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Clone + Send + 'static,
        S::Future: Send,
    {
        let result = self.initialize(inner, parts, context).await;
        let outcome = match &result {
            Ok(State::Session(_)) => "session",
            Ok(State::Stateless { .. }) => "stateless",
            Err(HandshakeError::Refused(_)) => "refused",
            Err(HandshakeError::Failed(_)) => "failed",
        };
        u64_counter!(
            "apollo.router.connectors.mcp.handshakes",
            "Number of MCP session handshakes the router made for connector sources",
            1,
            "connector.source" = self.source.clone(),
            "mcp.handshake.reason" = reason.as_str(),
            "mcp.handshake.outcome" = outcome
        );
        result
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
        let request = handshake_request(parts, &initialize, None, None, context);
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
        if version.as_str() >= STATELESS_SINCE {
            return Ok(State::Stateless { version: None });
        }
        let version = HeaderValue::try_from(version).map_err(BoxError::from)?;

        // The lifecycle requires `initialized` after a successful `initialize`, with or without a
        // session, and the negotiated version on it and on every request after it.
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let request = handshake_request(
            parts,
            &initialized,
            session_id.as_ref(),
            Some(&version),
            context,
        );
        let response = inner.oneshot(request).await?;
        if !response.http_response.status().is_success() {
            return Err(HandshakeError::Refused(Box::new(response)));
        }
        let Some(session_id) = session_id else {
            tracing::debug!(source = %self.source, "MCP server issued no session; treating it as stateless");
            return Ok(State::Stateless {
                version: Some(version),
            });
        };
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

/// Whether `request` carries credentials other than `opened_by`'s.
fn credentials_changed(opened_by: &HeaderMap, request: &HeaderMap) -> bool {
    DEFAULT_IDENTITY_HEADERS.iter().any(|name| {
        !opened_by
            .get_all(name)
            .iter()
            .eq(request.get_all(name).iter())
    })
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
    session: Option<&HeaderValue>,
    version: Option<&HeaderValue>,
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
    if let Some(id) = session {
        parts.headers.insert(SESSION_ID, id.clone());
    }
    if let Some(version) = version {
        parts.headers.insert(PROTOCOL_VERSION, version.clone());
    }
    HttpRequest {
        http_request: http::Request::from_parts(parts, body::from_bytes(message.to_string())),
        context: context.clone(),
    }
}

/// Reads `body` until it holds a JSON-RPC response, returning what was read and that response.
/// An event stream can stay open after the response, so it is not read to the end: it is decoded
/// as it arrives, with the same SSE decoder as connector responses, and reading stops at the
/// first complete event that holds a response.
async fn first_reply(
    mut body: RouterBody,
    event_stream: bool,
) -> Result<(Bytes, Option<Value>), BoxError> {
    let mut read = BytesMut::new();
    let mut decoder = StreamFraming::ServerSentEvents.decoder();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            read.extend_from_slice(&data);
            if read.len() > MAX_HANDSHAKE_BODY {
                return Err("MCP initialize response is too large".into());
            }
            if event_stream
                && let Some(reply) = decoder
                    .push(&data)
                    .iter()
                    .filter_map(|event| serde_json::from_str::<Value>(&event.data).ok())
                    .find(is_reply)
            {
                return Ok((read.freeze(), Some(reply)));
            }
        }
    }
    let reply = if event_stream {
        decoder
            .finish()
            .iter()
            .filter_map(|event| serde_json::from_str::<Value>(&event.data).ok())
            .find(is_reply)
    } else {
        serde_json::from_slice(&read).ok()
    };
    Ok((read.freeze(), reply))
}

/// Whether `message` is a JSON-RPC response rather than a request or notification.
fn is_reply(message: &Value) -> bool {
    message.get("method").is_none()
        && (message.get("result").is_some() || message.get("error").is_some())
}

/// `response`, with a body that keeps `slot`, and so its session, alive until the body has been
/// read or dropped.
fn hold_while_reading(response: HttpResponse, slot: Arc<Slot>) -> HttpResponse {
    let HttpResponse {
        http_response,
        context,
    } = response;
    let (parts, body) = http_response.into_parts();
    let chunks = BodyDataStream::new(body).map(move |chunk| {
        let _lease = &slot;
        chunk
    });
    HttpResponse {
        http_response: http::Response::from_parts(parts, body::from_result_stream(chunks)),
        context,
    }
}

/// Ends `session` with the credentials that opened it. This is a courtesy that frees the
/// server's resources early; its outcome is ignored.
fn close<S>(session: &Session, inner: S)
where
    S: Service<HttpRequest, Response = HttpResponse, Error = BoxError> + Send + 'static,
    S::Future: Send,
{
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
    // A layer dropped outside the runtime, at the very end of shutdown, can't close anything.
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = inner.oneshot(request).await;
        });
    }
}

#[cfg(test)]
mod tests;
