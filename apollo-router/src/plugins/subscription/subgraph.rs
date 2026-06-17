//! Subgraph-side implementation of subscriptions.
//!
//! Tests for this functionality are still mostly in the `crate::services::subgraph_service::tests` module.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures::SinkExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use http::HeaderValue;
use serde::Serialize;
use tokio::select;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::BoxError;
use tracing::Instrument;
use uuid::Uuid;

use super::callback::create_verifier;
use super::notification::Notify;
use crate::Context;
use crate::context::OPERATION_NAME;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::metrics::FutureMetricsExt;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::subscription::CallbackMode;
use crate::plugins::subscription::SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::SubscriptionMode;
use crate::plugins::subscription::WebSocketConfiguration;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::log_subgraph_request_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::otel::span_ext::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::otel::prepare_context;
use crate::protocols::websocket::BoxSubscriptionStream;
use crate::protocols::websocket::GraphqlWebSocket;
use crate::protocols::websocket::SubscriptionEvent;
use crate::protocols::websocket::convert_websocket_stream;
use crate::services::OperationKind;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;

static CALLBACK_PROTOCOL_ACCEPT: HeaderValue =
    HeaderValue::from_static("application/json;callbackSpec=1.0");

pub(crate) struct SubscriptionSubgraphLayer {
    notify: Notify<String, graphql::Response>,
    subscription_config: Option<Arc<SubscriptionConfig>>,
    service_name: Arc<str>,
}

impl SubscriptionSubgraphLayer {
    pub(crate) fn new(
        notify: Notify<String, graphql::Response>,
        subscription_config: Option<Arc<SubscriptionConfig>>,
        service_name: Arc<str>,
    ) -> Self {
        Self {
            notify,
            subscription_config,
            service_name,
        }
    }
}

impl<S> tower::Layer<S> for SubscriptionSubgraphLayer {
    type Service = SubscriptionSubgraphService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SubscriptionSubgraphService {
            notify: self.notify.clone(),
            subscription_config: self.subscription_config.clone(),
            service_name: self.service_name.clone(),
            inner,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubscriptionSubgraphService<S> {
    notify: Notify<String, graphql::Response>,
    subscription_config: Option<Arc<SubscriptionConfig>>,
    service_name: Arc<str>,
    inner: S,
}

impl<S> tower::Service<SubgraphRequest> for SubscriptionSubgraphService<S>
where
    S: tower::Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: SubgraphRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let notify = self.notify.clone();
        let subscription_config = self.subscription_config.clone();
        let service_name = self.service_name.clone();

        Box::pin(async move {
            match subgraph_request(notify, req, subscription_config, &service_name).await? {
                ControlFlow::Continue(request) => inner.call(request).await,
                ControlFlow::Break(response) => Ok(response),
            }
        })
    }
}

#[cfg_attr(test, derive(serde::Deserialize))]
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubscriptionExtension {
    pub(crate) subscription_id: String,
    pub(crate) callback_url: url::Url,
    pub(crate) verifier: String,
    pub(crate) heartbeat_interval_ms: u64,
}

/// The initial-connection inputs needed to re-establish a dropped WebSocket subscription, bundled
/// as a single `Option<ReconnectInputs>` so they are conditionally present together — adding a new
/// reconnect input in the future only requires updating this struct, not adding another
/// `Option<T>` with its own `unreachable!`.
///
/// This holds only the pieces the reconnect path actually re-sends — the request URI, headers, and
/// body — rather than the whole `http::Request`. In particular it omits the request extensions,
/// which the WebSocket upgrade path never forwards and which would otherwise duplicate
/// `signing_params` (extracted from them) and pin unrelated request context for the whole
/// subscription lifetime.
struct ReconnectInputs {
    uri: http::Uri,
    headers: http::HeaderMap,
    body: graphql::Request,
    connection_params: Option<serde_json_bytes::Value>,
    signing_params: Option<Arc<SigningParamsConfig>>,
    subscription_hash: String,
    subgraph_cfg: WebSocketConfiguration,
}

/// Set up a subscription with the subgraph over a WebSocket protocol
async fn call_websocket(
    mut notify: Notify<String, graphql::Response>,
    request: SubgraphRequest,
    context: Context,
    service_name: &str,
    subgraph_cfg: &WebSocketConfiguration,
    subscription_hash: String,
) -> Result<SubgraphResponse, BoxError> {
    let subgraph_request_event = context
        .extensions()
        .with_lock(|lock| lock.get::<SubgraphEventRequest>().cloned());
    let log_request_level = subgraph_request_event.and_then(|s| {
        if s.condition.lock().evaluate_request(&request) == Some(true) {
            Some(s.level)
        } else {
            None
        }
    });

    let SubgraphRequest {
        subgraph_request,
        subscription_stream,
        id: subgraph_request_id,
        ..
    } = request;
    let subscription_stream_tx =
        subscription_stream.ok_or_else(|| FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: "cannot get the websocket stream".to_string(),
        })?;
    let supergraph_operation_name = context.get::<_, String>(OPERATION_NAME).ok().flatten();
    // In passthrough mode, we maintain persistent WebSocket connections and need the
    // subscription_closing_signal to properly clean up long-running forwarding tasks
    // when subscriptions are terminated (see tokio::select! usage below).
    //
    // Websocket subscriptions are closed when:
    // * The closing signal is received from the subgraph.
    // * The connection to the subgraph is severed.
    //
    // The reason that we need the subscription closing signal is that deduplication will
    // cause multiple client subscriptions to listen to the same source subscription. Therefore we
    // must not close the subscription if a single connection is dropped. Only when ALL connections are dropped.
    // Conversely, if the connection between router and subgraph is closed, ALL client subscription connections
    // are dropped immediately.
    let (handle, created, mut subscription_closing_signal) = notify
        .create_or_subscribe(subscription_hash.clone(), false, supergraph_operation_name)
        .await?;
    u64_counter!(
        "apollo.router.operations.subscriptions",
        "Total requests with subscription operations",
        1,
        subscriptions.mode = "passthrough",
        subscriptions.deduplicated = !created,
        subgraph.service.name = service_name.to_string()
    );
    if !created {
        subscription_stream_tx
            .send(Box::pin(handle.into_stream()))
            .await?;

        // Dedup happens here
        return Ok(SubgraphResponse::builder()
            .context(context)
            .subgraph_name(service_name)
            .extensions(Object::default())
            .build());
    }

    let max_reconnect_attempts = subgraph_cfg.max_reconnect_attempts;

    let (parts, body) = subgraph_request.into_parts();

    // Check context key and Authorization header (context key takes precedence) to set connection params if needed
    let connection_params = match (
        context.get_json_value(SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS),
        parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|auth| auth.to_str().ok()),
    ) {
        (Some(connection_params), _) => Some(connection_params),
        (None, Some(authorization)) => Some(serde_json_bytes::json!({ "token": authorization })),
        _ => None,
    };

    // Extract before passing the URI/headers to the helper, which consumes them. Headers are
    // forwarded to the WebSocket upgrade request; extensions are not.
    let signing_params = parts.extensions.get::<Arc<SigningParamsConfig>>().cloned();

    // Bundle the reconnect inputs into a single Option, cloning only the pieces the reconnect path
    // re-sends and only when reconnection is enabled. Populated together, so adding a new reconnect
    // input only requires updating ReconnectInputs — not a separate Option<T> with its own
    // unreachable!.
    let retry_inputs = (max_reconnect_attempts > 0).then(|| ReconnectInputs {
        uri: parts.uri.clone(),
        headers: parts.headers.clone(),
        body: body.clone(),
        connection_params: connection_params.clone(),
        signing_params: signing_params.clone(),
        subscription_hash: subscription_hash.clone(),
        subgraph_cfg: subgraph_cfg.clone(),
    });

    let (gql_stream, resp) = open_ws_gql_stream(
        service_name,
        &context,
        parts.uri,
        parts.headers,
        body,
        connection_params,
        signing_params,
        subgraph_cfg,
        subscription_hash,
        log_request_level,
        false,
    )
    .await?;

    let (mut handle_sink, handle_stream) = handle.split();
    let service_name_for_task = service_name.to_string();
    let reconnect_delay = subgraph_cfg
        .reconnect_delay
        .unwrap_or(Duration::from_secs(1));
    // A reconnected connection that stays open at least this long is treated as
    // stable, and the per-disconnect retry budget refreshes. A subgraph that
    // flaps (accepts the handshake then drops faster than this) keeps burning
    // through the current budget and eventually terminates the subscription.
    // Formula: 5 × reconnect_delay, clamped to [500 ms, 60 s]. The 5× multiplier is
    // intentionally undocumented in user-facing config because it scales with
    // whatever delay an operator chose — the stability check always requires the
    // connection to have survived at least a few retry cycles. The 500 ms floor
    // guards against `reconnect_delay: 0s` collapsing the grace window to zero
    // (every elapsed >= 0 → every drop resets the budget → unbounded loop). The 60 s
    // ceiling guards the opposite extreme: with a large `reconnect_delay` (e.g. 120s),
    // 5× would demand the connection survive 10 minutes to count as stable, so a
    // connection that legitimately ran for minutes would never refresh the budget and
    // `max_reconnect_attempts` would silently become a lifetime cap. A connection that
    // stays open a full minute is stable by any practical measure.
    let stability_grace = reconnect_delay
        .saturating_mul(5)
        .clamp(Duration::from_millis(500), Duration::from_secs(60));

    // Forward GraphQL subscription stream to WebSocket handle, with optional reconnection on
    // connection drop. Connection lifecycle is managed by the WebSocket infrastructure, so we
    // don't need to handle connection_closed_signal here.
    // Use a dedicated span for the long-lived forwarding task. `info_span!` parents to the
    // current (subgraph request) span by default, so reconnect handshakes stay chained to the
    // originating trace — but because a child holds its parent only by id, the request span is
    // free to close when `call_websocket` returns rather than being held open (and unexported)
    // for the entire subscription lifetime.
    let forwarding_span = tracing::info_span!(
        "subscription_forwarding",
        "apollo.subgraph.name" = %service_name,
    );
    // The forwarding task outlives `call_websocket`, so it needs its own handle to the context for
    // masking request headers on each reconnect's `subgraph.request` log event.
    let task_context = context.clone();
    let forwarding_task = tokio::task::spawn(
        async move {
            let mut gql_stream = gql_stream;
            let mut attempt = 0u32;

            'retry: loop {
                let connection_started_at = Instant::now();
                // Read events from the current stream and forward them to clients. The loop yields
                // the error from the connection drop (`Disconnected`) that ends it; every other
                // way out of the read phase is a terminal `break 'retry`.
                let disconnect_error: graphql::Response = loop {
                    select! {
                        // We prefer to specify the order of checks within the select
                        biased;
                        // gql_stream is the stream opened from router to subgraph to receive events.
                        // handle_sink broadcasts those events to all subscribed router clients.
                        // This arm is checked first so that buffered items are drained before
                        // acting on a closing signal that arrived simultaneously.
                        item = gql_stream.next() => {
                            match item {
                                Some(SubscriptionEvent::Payload(resp)) => {
                                    // Subscription data or a genuine subgraph operation error:
                                    // forward to all subscribed router clients.
                                    if handle_sink.send_sync(resp).is_err() {
                                        // All router clients have disconnected; no need to keep
                                        // the subgraph connection open. We don't increment the
                                        // subgraph-ended counter here because the clients left
                                        // first.
                                        break 'retry;
                                    }
                                }
                                Some(SubscriptionEvent::TransientError(resp)) => {
                                    // A transport error on a connection that is still readable (a
                                    // message that failed to deserialize). When reconnect is
                                    // configured, swallow it: forwarding it would set
                                    // `subscribed=false` on the client response and tear down
                                    // HTTP-multipart subscribers. With reconnect disabled, forward
                                    // it (preserving the pre-reconnect behaviour). Either way the
                                    // connection stays open, so keep reading.
                                    if max_reconnect_attempts > 0 {
                                        tracing::debug!(
                                            "suppressing transient subgraph transport error during reconnect window"
                                        );
                                        // Suppressing forwards nothing, so the `send_sync`
                                        // client-departure check is skipped; and a continuous flood
                                        // of these would keep the biased `gql_stream.next()` arm
                                        // ready and starve the closing-signal arm. Poll the closing
                                        // signal explicitly so an all-clients-gone teardown is still
                                        // observed mid-flood. `Empty` means keep going; any other
                                        // outcome (a queued close, sender dropped, or lagged) means
                                        // stop serving.
                                        if !matches!(
                                            subscription_closing_signal.try_recv(),
                                            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                                        ) {
                                            tracing::debug!(
                                                "subscription_closing_signal observed while suppressing transient errors"
                                            );
                                            break 'retry;
                                        }
                                    } else if handle_sink.send_sync(resp).is_err() {
                                        break 'retry;
                                    }
                                }
                                Some(SubscriptionEvent::Disconnected(err)) => {
                                    // The subgraph connection dropped. If the just-dropped
                                    // connection stayed open past the grace window, treat it as
                                    // stable and refresh the retry budget so each disconnect of a
                                    // long-lived subscription gets its own `max_reconnect_attempts`.
                                    // Quick-flapping connections fall through and keep accumulating
                                    // against the existing budget.
                                    if connection_started_at.elapsed() >= stability_grace {
                                        attempt = 0;
                                    }
                                    tracing::debug!("subscription WebSocket connection dropped");
                                    // Hand the drop error to the reconnect logic; it is forwarded
                                    // to clients only if reconnection is ultimately exhausted.
                                    break err;
                                }
                                None => {
                                    // The stream ends with `None` only on a terminal,
                                    // non-recoverable end: a protocol-level Complete, a genuine
                                    // operation error (already forwarded above), or a protocol
                                    // violation. Don't reconnect.
                                    tracing::debug!("gql_stream completed normally");
                                    increment_subgraph_ended_counter(&service_name_for_task);
                                    break 'retry;
                                }
                            }
                        },
                        // This branch handles subscription termination signals. Unlike callback
                        // mode, passthrough mode maintains persistent connections that require
                        // explicit cleanup. We don't increment any metrics here because the
                        // subscription was ended by all clients disconnecting.
                        // The signal channel only ever carries a single close `()`, so every
                        // `recv()` outcome means "stop serving": `Ok` is the close, `Err(Closed)`
                        // means the sender (topic) is gone, and `Err(Lagged)` means the close was
                        // sent and missed. Breaking on any of them is correct.
                        _ = subscription_closing_signal.recv() => {
                            tracing::debug!("subscription_closing_signal triggered");
                            break 'retry;
                        },
                    }
                };

                // The subgraph connection dropped. Reconnect if we have attempts remaining.
                // Loop until reconnect succeeds or attempts are exhausted.
                'reconnect: loop {
                    if attempt < max_reconnect_attempts {
                        attempt += 1;
                        tracing::debug!(
                            attempt,
                            max_reconnect_attempts,
                            "subscription WebSocket connection dropped, reconnecting"
                        );
                        // Reconnect inputs are populated whenever max_reconnect_attempts > 0,
                        // which is the only way `attempt < max_reconnect_attempts` can be true.
                        let Some(inputs) = &retry_inputs else {
                            unreachable!(
                                "reconnect inputs are populated whenever max_reconnect_attempts > 0"
                            );
                        };
                        // Abort the reconnect if all router clients drop during the delay,
                        // otherwise we'd reconnect to the subgraph for nobody.
                        select! {
                            biased;
                            _ = subscription_closing_signal.recv() => {
                                tracing::debug!("subscription_closing_signal received during reconnect delay");
                                break 'retry;
                            },
                            _ = tokio::time::sleep(reconnect_delay) => {},
                        }
                        // The handshake (TCP + TLS + ConnectionAck) can take seconds. If all
                        // clients drop during it, abort rather than completing a fresh subgraph
                        // subscription that will immediately need to be torn down.
                        let handshake_result = select! {
                            biased;
                            _ = subscription_closing_signal.recv() => {
                                tracing::debug!("subscription_closing_signal received during reconnect handshake");
                                break 'retry;
                            },
                            res = open_ws_gql_stream(
                                &service_name_for_task,
                                &task_context,
                                inputs.uri.clone(),
                                inputs.headers.clone(),
                                inputs.body.clone(),
                                inputs.connection_params.clone(),
                                inputs.signing_params.clone(),
                                &inputs.subgraph_cfg,
                                inputs.subscription_hash.clone(),
                                log_request_level,
                                true,
                            ) => res,
                        };
                        // Count only attempts that actually issued a handshake. A closing signal
                        // during the reconnect delay or during the handshake itself breaks out of
                        // 'retry above without ever completing `open_ws_gql_stream`, so it is not
                        // charged to this counter. Both successful and failed handshakes count.
                        u64_counter!(
                            "apollo.router.operations.subscriptions.reconnect",
                            "Number of subscription WebSocket reconnect attempts",
                            1,
                            subgraph.name = service_name_for_task.clone()
                        );
                        match handshake_result {
                            Ok((new_stream, _resp)) => {
                                gql_stream = new_stream;
                                break 'reconnect;
                            }
                            Err(err) => {
                                tracing::error!(
                                    "failed to reconnect subscription WebSocket (attempt {attempt}/{max_reconnect_attempts}): {err}"
                                );
                                // Continue to next attempt rather than giving up immediately.
                            }
                        }
                    } else {
                        // Reconnection exhausted. Surface the error from the connection drop that
                        // triggered this reconnect cycle so a failed subscription is
                        // distinguishable from a normal completion, then end the stream via
                        // handle_sink.close() below.
                        let _ = handle_sink.send_sync(disconnect_error);
                        increment_subgraph_ended_counter(&service_name_for_task);
                        break 'retry;
                    }
                }
            }
            // Emit a single completion event for the logical subscription, on any teardown
            // (subgraph end, reconnect exhausted, or all clients disconnecting) — matching the
            // pre-reconnect behaviour, which emitted this once whenever the physical connection's
            // forwarding ended. It lives here (not in SubscriptionStream, which runs once per
            // physical connection) so a reconnecting subscription is counted once total rather than
            // once per reconnect. Note this is distinct from `terminated.subgraph`
            // (increment_subgraph_ended_counter), which is intentionally recorded only when the
            // subgraph — not a client — ends the subscription.
            u64_counter!(
                "apollo.router.operations.subscriptions.events",
                "Number of subscription events",
                1,
                subscriptions.mode = "passthrough",
                subscriptions.complete = true
            );

            // Send ForceDelete to the pubsub so the client-facing HandleStream receives None
            // and terminates. Without this, the HandleStream waits forever when the subgraph
            // closes the WebSocket and there are no reconnect attempts left.
            let _ = handle_sink.close().await;
        }
        .with_current_meter_provider()
        .instrument(forwarding_span),
    );

    // Hand the client-facing stream to the caller. If the receiver was dropped between spawning
    // the forwarding task and this send (e.g. the request was cancelled), abort the task so it
    // doesn't keep the subgraph WebSocket open for a subscription nobody is listening to. The
    // abort drops `gql_stream` (closing the WS) and `handle_sink` (tearing down the topic).
    if let Err(err) = subscription_stream_tx.send(Box::pin(handle_stream)).await {
        forwarding_task.abort();
        return Err(err.into());
    }

    Ok(SubgraphResponse::new_from_response(
        resp.map(|_| graphql::Response::default()),
        context,
        service_name.to_string(),
        subgraph_request_id,
    ))
}

fn get_websocket_request(
    service_name: &str,
    uri: &http::Uri,
    mut headers: http::HeaderMap,
    subgraph_ws_cfg: &WebSocketConfiguration,
) -> Result<http::Request<()>, FetchError> {
    let mut subgraph_url = url::Url::parse(&uri.to_string()).map_err(|err| {
        tracing::error!("cannot parse subgraph url {}: {err:?}", uri);
        FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: "cannot parse subgraph url".to_string(),
        }
    })?;
    let new_scheme = match subgraph_url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => "ws",
    };
    subgraph_url.set_scheme(new_scheme).map_err(|err| {
        tracing::error!("cannot set a scheme '{new_scheme}' on subgraph url: {err:?}");

        FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: "cannot set a scheme on websocket url".to_string(),
        }
    })?;

    let subgraph_url = match &subgraph_ws_cfg.path {
        Some(path) => subgraph_url
            .join(path)
            .map_err(|_| FetchError::SubrequestWsError {
                service: service_name.to_string(),
                reason: "cannot parse subgraph url with the specific websocket path".to_string(),
            })?,
        None => subgraph_url,
    };
    // XXX During hyper upgrade, observed that we had lost the implementation for Url
    // so I made the expedient decision to get a string representation (as_str())
    // for the creation of the client request. This works fine, but I'm not sure
    // why we need to do it, because into_client_request **should** be implemented
    // for Url...
    let mut request = subgraph_url.as_str().into_client_request().map_err(|err| {
        tracing::error!("cannot create websocket client request: {err:?}");

        FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: "cannot create websocket client request".to_string(),
        }
    })?;
    request.headers_mut().insert(
        http::header::SEC_WEBSOCKET_PROTOCOL,
        subgraph_ws_cfg.protocol.into(),
    );
    headers.extend(request.headers_mut().drain());
    *request.headers_mut() = headers;

    // Inject trace propagation headers into the WebSocket upgrade request
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(
            &prepare_context(tracing::Span::current().context()),
            &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
        );
    });

    Ok(request)
}

/// Set up a subscription with the subgraph over the callback protocol
async fn setup_callback(
    mut notify: Notify<String, graphql::Response>,
    request: &mut SubgraphRequest,
    context: Context,
    service_name: &str,
    config: &CallbackMode,
    subscription_id: String,
) -> Result<ControlFlow<SubgraphResponse>, BoxError> {
    let operation_name = context.get::<_, String>(OPERATION_NAME).ok().flatten();
    // Call create_or_subscribe on notify
    // Note: _subscription_closing_signal is intentionally unused in callback mode.
    // In callback mode, subscriptions are managed via HTTP callbacks rather than
    // persistent connections, so there's no long-running task that needs to be
    // notified when the subscription closes (unlike passthrough mode which uses
    // the signal to clean up WebSocket forwarding tasks).
    //
    // Callback subscriptions are closed when the subgraph returns 404
    let (handle, created, _subscription_closing_signal) = notify
        .create_or_subscribe(subscription_id.clone(), true, operation_name)
        .await?;

    // If it existed before just send the right stream (handle) and early return
    let stream_tx =
        request
            .subscription_stream
            .clone()
            .ok_or_else(|| FetchError::SubrequestWsError {
                service: service_name.to_string(),
                reason: "cannot get the callback stream".to_string(),
            })?;
    stream_tx.send(Box::pin(handle.into_stream())).await?;

    u64_counter!(
        "apollo.router.operations.subscriptions",
        "Total requests with subscription operations",
        1,
        subscriptions.mode = "callback",
        subscriptions.deduplicated = !created,
        subgraph.name = service_name.to_string()
    );
    if !created {
        // Dedup happens here
        return Ok(ControlFlow::Break(
            SubgraphResponse::builder()
                .subgraph_name(service_name)
                .context(context)
                .extensions(Object::default())
                .build(),
        ));
    }

    // If not then put the subscription_id in the extensions for callback mode and continue
    // Do this if the topic doesn't already exist
    let mut callback_url = config.public_url.clone();
    if callback_url.path_segments_mut().is_err() {
        callback_url = callback_url.join(&subscription_id)?;
    } else {
        callback_url
            .path_segments_mut()
            .expect("can't happen because we checked before")
            .push(&subscription_id);
    }

    // Generate verifier
    let verifier =
        create_verifier(&subscription_id).map_err(|err| FetchError::SubrequestHttpError {
            service: service_name.to_string(),
            reason: format!("{err:?}"),
            status_code: None,
        })?;
    request
        .subgraph_request
        .headers_mut()
        .append(http::header::ACCEPT, CALLBACK_PROTOCOL_ACCEPT.clone());

    let subscription_extension = SubscriptionExtension {
        subscription_id,
        callback_url,
        verifier,
        heartbeat_interval_ms: config
            .heartbeat_interval
            .into_option()
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0),
    };
    request.subgraph_request.body_mut().extensions.insert(
        "subscription",
        serde_json_bytes::to_value(subscription_extension).map_err(|err| {
            FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                reason: format!("cannot serialize the subscription extension: {err:?}",),
                status_code: None,
            }
        })?,
    );

    Ok(ControlFlow::Continue(()))
}

async fn subgraph_request(
    notify: Notify<String, graphql::Response>,
    mut request: SubgraphRequest,
    subscription_config: Option<Arc<SubscriptionConfig>>,
    service_name: &str,
) -> Result<ControlFlow<SubgraphResponse, SubgraphRequest>, BoxError> {
    if request.operation_kind == OperationKind::Subscription
        && request.subscription_stream.is_some()
    {
        let subscription_config =
            subscription_config.ok_or_else(|| FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                reason: "subscription is not enabled".to_string(),
                status_code: None,
            })?;
        let mode = subscription_config.mode.get_subgraph_config(service_name);
        let context = request.context.clone();

        let dedup = subscription_config.deduplication.get(service_name);
        let hashed_request = if dedup.enabled {
            request.to_sha256(&dedup.ignored_headers, dedup.ignore_auth_context)
        } else {
            Uuid::new_v4().to_string()
        };

        match &mode {
            Some(SubscriptionMode::Passthrough(ws_conf)) => {
                // call_websocket for passthrough mode
                return call_websocket(
                    notify,
                    request,
                    context,
                    service_name,
                    ws_conf,
                    hashed_request,
                )
                .await
                .map(ControlFlow::Break);
            }
            Some(SubscriptionMode::Callback(callback_conf)) => {
                // This will modify the body to add `extensions` for the callback
                // subscription protocol.
                let control = setup_callback(
                    notify,
                    &mut request,
                    context.clone(),
                    service_name,
                    callback_conf,
                    hashed_request,
                )
                .await?;

                if let ControlFlow::Break(response) = control {
                    return Ok(ControlFlow::Break(response));
                }
            }
            _ => {
                return Err(Box::new(FetchError::SubrequestWsError {
                    service: service_name.to_string(),
                    reason: "subscription mode is not enabled".to_string(),
                }));
            }
        }

        Ok(ControlFlow::Continue(request))
    } else {
        Ok(ControlFlow::Continue(request))
    }
}

/// Open a WebSocket subscription stream to a subgraph. Shared by the initial
/// connect path in `call_websocket` and the reconnect retry path so that the
/// signing, logging, connection, and protocol setup live in one place.
#[allow(clippy::too_many_arguments)]
async fn open_ws_gql_stream(
    service_name: &str,
    context: &Context,
    uri: http::Uri,
    headers: http::HeaderMap,
    body: graphql::Request,
    connection_params: Option<serde_json_bytes::Value>,
    signing_params: Option<Arc<SigningParamsConfig>>,
    subgraph_cfg: &WebSocketConfiguration,
    subscription_hash: String,
    log_request_level: Option<EventLevel>,
    // When true, this is a reconnect attempt rather than the initial connect. Handshake failures
    // are then counted by the reconnect machinery's own metric, so they must not also increment
    // `apollo.router.operations.subscriptions.rejected`, which tracks rejected *subscription
    // requests*, not reconnect failures.
    is_reconnect: bool,
) -> Result<(BoxSubscriptionStream, http::Response<Option<Vec<u8>>>), BoxError> {
    let request = get_websocket_request(service_name, &uri, headers, subgraph_cfg)?;

    let request = if let Some(signing_params) = signing_params {
        signing_params.sign_empty(request, service_name).await?
    } else {
        request
    };

    if let Some(level) = log_request_level {
        log_subgraph_request_event(
            level,
            service_name,
            crate::services::header_masking::masked_headers_for_log(
                context,
                crate::services::header_masking::Direction::Request,
                Some(service_name),
                request.headers(),
            ),
            request.method(),
            request.version(),
            serde_json::to_string(&body).unwrap_or_default(),
            &format!("Websocket request body to subgraph {service_name:?}"),
        );
    }

    let uri = request.uri();
    let path = uri.path();
    let host = uri.host().unwrap_or_default();
    let port = uri.port_u16().unwrap_or_else(|| {
        let scheme = uri.scheme_str();
        if scheme == Some("wss") {
            443
        } else if scheme == Some("ws") {
            80
        } else {
            0
        }
    });

    let subgraph_req_span = tracing::info_span!(SUBGRAPH_REQUEST_SPAN_NAME,
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %service_name,
        "graphql.operation.name" = body.operation_name.as_deref().unwrap_or(""),
    );

    let (ws_stream, resp) = match request.uri().scheme_str() {
        Some("wss") => {
            connect_async_tls_with_config(request, None, false, None)
                .instrument(subgraph_req_span)
                .await
        }
        _ => connect_async(request).instrument(subgraph_req_span).await,
    }
    .map_err(|err| {
        let error_details = match &err {
            tokio_tungstenite::tungstenite::Error::Utf8(details) => {
                format!("invalid UTF-8 in WebSocket handshake: {details}")
            }
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                let status = response.status();
                let headers = response
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        let header_value = v.to_str().unwrap_or("HTTP Error");
                        format!("{k:?}: {header_value:?}")
                    })
                    .collect::<Vec<String>>()
                    .join("; ");
                format!("WebSocket upgrade failed. Status: {status}; Headers: [{headers}]")
            }
            tokio_tungstenite::tungstenite::Error::Protocol(proto_err) => {
                format!("WebSocket protocol error: {proto_err}")
            }
            other_error => other_error.to_string(),
        };

        tracing::debug!(
            error.type   = "websocket_connection_failed",
            error.details= %error_details,
            error.source = %std::any::type_name_of_val(&err),
            "WebSocket connection failed"
        );

        if !is_reconnect {
            increment_subgraph_rejected_counter(service_name);
        }
        FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: format!("cannot connect websocket to subgraph: {error_details}"),
        }
    })?;

    let gql_socket = GraphqlWebSocket::new(
        convert_websocket_stream(ws_stream, subscription_hash.clone()),
        subscription_hash,
        subgraph_cfg.protocol,
        connection_params,
    )
    .await
    .map_err(|err| {
        if !is_reconnect {
            increment_subgraph_rejected_counter(service_name);
        }
        FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: format!("cannot get the GraphQL websocket stream: {}", err.message),
        }
    })?;

    let gql_stream = gql_socket
        .into_subscription(body, subgraph_cfg.heartbeat_interval.into_option())
        .await
        .map_err(|err| {
            if !is_reconnect {
                increment_subgraph_rejected_counter(service_name);
            }
            FetchError::SubrequestWsError {
                service: service_name.to_string(),
                reason: format!("cannot send the subgraph request to websocket stream: {err:?}"),
            }
        })?;

    Ok((Box::pin(gql_stream), resp))
}

fn increment_subgraph_rejected_counter(service_name: &str) {
    u64_counter!(
        "apollo.router.operations.subscriptions.rejected",
        "Number of subscription requests rejected",
        1,
        reason = "subgraph",
        subgraph.name = service_name.to_string()
    );
}

fn increment_subgraph_ended_counter(service_name: &str) {
    u64_counter!(
        "apollo.router.operations.subscriptions.terminated.subgraph",
        "Number of subscriptions ended by the subgraph closing the WebSocket connection",
        1,
        subgraph.name = service_name.to_string()
    );
}
