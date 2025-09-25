//! Subgraph-side implementation of subscriptions.
//!
//! Tests for this functionality are still mostly in the `crate::services::subgraph_service::tests` module.

use std::ops::ControlFlow;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use http::HeaderValue;
use opentelemetry::Key;
use opentelemetry::KeyValue;
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
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::subscription::CallbackMode;
use crate::plugins::subscription::SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::SubscriptionMode;
use crate::plugins::subscription::WebSocketConfiguration;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::protocols::websocket::GraphqlWebSocket;
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

    let request = get_websocket_request(service_name, parts, subgraph_cfg)?;

    let signing_params = context
        .extensions()
        .with_lock(|lock| lock.get::<Arc<SigningParamsConfig>>().cloned());

    let request = if let Some(signing_params) = signing_params {
        signing_params.sign_empty(request, service_name).await?
    } else {
        request
    };

    if let Some(level) = log_request_level {
        let mut attrs = Vec::with_capacity(5);
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.headers"),
            opentelemetry::Value::String(format!("{:?}", request.headers()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.method"),
            opentelemetry::Value::String(format!("{}", request.method()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.version"),
            opentelemetry::Value::String(format!("{:?}", request.version()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.body"),
            opentelemetry::Value::String(
                serde_json::to_string(request.body())
                    .unwrap_or_default()
                    .into(),
            ),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("subgraph.name"),
            opentelemetry::Value::String(service_name.to_string().into()),
        ));
        log_event(
            level,
            "subgraph.request",
            attrs,
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
    .map_err(|err| FetchError::SubrequestWsError {
        service: service_name.to_string(),
        reason: format!("cannot get the GraphQL websocket stream: {}", err.message),
    })?;

    let gql_stream = gql_socket
        .into_subscription(body, subgraph_cfg.heartbeat_interval.into_option())
        .await
        .map_err(|err| FetchError::SubrequestWsError {
            service: service_name.to_string(),
            reason: format!("cannot send the subgraph request to websocket stream: {err:?}"),
        })?;

    let (handle_sink, handle_stream) = handle.split();
    // Forward GraphQL subscription stream to WebSocket handle
    // Connection lifecycle is managed by the WebSocket infrastructure,
    // so we don't need to handle connection_closed_signal here
    tokio::task::spawn(async move {
        select! {
            // We prefer to specify the order of checks within the select
            biased;
            // gql_stream is the stream opened from router to subgraph to receive events
            // handle_sink is just a broadcast sender to send the events received from subgraphs to the router's client
            // if all router's clients are closed the sink will be closed too and then the .forward future will end
            // It will then also trigger poll_close on the gql_stream which will initiate the termination process (like properly closing ws connection cf protocols/websocket.rs)
            _ = gql_stream
                .map(Ok::<_, graphql::Error>)
                .forward(handle_sink) => {
                tracing::debug!("gql_stream empty");
            },
            // This branch handles subscription termination signals. Unlike callback mode,
            // passthrough mode maintains persistent connections that require explicit cleanup.
            _ = subscription_closing_signal.recv() => {
                tracing::debug!("subscription_closing_signal triggered");
            }
        }
    });

    subscription_stream_tx.send(Box::pin(handle_stream)).await?;

    Ok(SubgraphResponse::new_from_response(
        resp.map(|_| graphql::Response::default()),
        context,
        service_name.to_string(),
        subgraph_request_id,
    ))
}

fn get_websocket_request(
    service_name: &str,
    mut parts: http::request::Parts,
    subgraph_ws_cfg: &WebSocketConfiguration,
) -> Result<http::Request<()>, FetchError> {
    let mut subgraph_url = url::Url::parse(&parts.uri.to_string()).map_err(|err| {
        tracing::error!("cannot parse subgraph url {}: {err:?}", parts.uri);
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
    parts.headers.extend(request.headers_mut().drain());
    *request.headers_mut() = parts.headers;

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
        subgraph.service.name = service_name.to_string()
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

        let hashed_request = if subscription_config.deduplication.enabled {
            request.to_sha256(&subscription_config.deduplication.ignored_headers)
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
