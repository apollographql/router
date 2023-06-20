//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::task::Poll;

use ::serde::Deserialize;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZlibEncoder;
use futures::future::BoxFuture;
use futures::SinkExt;
use futures::StreamExt;
use global::get_text_map_propagator;
use http::header::ACCEPT;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::{self};
use http::HeaderMap;
use http::HeaderValue;
use hyper::client::HttpConnector;
use hyper::Client;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use rustls::RootCertStore;
use schemars::JsonSchema;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_http::decompression::Decompression;
use tower_http::decompression::DecompressionLayer;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

use super::layers::content_negociation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use super::Plugins;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugins::subscription::create_verifier;
use crate::plugins::subscription::CallbackMode;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::SubscriptionMode;
use crate::plugins::subscription::WebSocketConfiguration;
use crate::plugins::subscription::SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;
use crate::protocols::websocket::convert_websocket_stream;
use crate::protocols::websocket::GraphqlWebSocket;
use crate::query_planner::OperationKind;
use crate::services::layers::apq;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Context;
use crate::Notify;

const PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_FOUND";
const PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_SUPPORTED";
const PERSISTED_QUERY_NOT_FOUND_MESSAGE: &str = "PersistedQueryNotFound";
const PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE: &str = "PersistedQueryNotSupported";
const CODE_STRING: &str = "code";
const PERSISTED_QUERY_KEY: &str = "persistedQuery";
const HASH_VERSION_KEY: &str = "version";
const HASH_VERSION_VALUE: i32 = 1;
const HASH_KEY: &str = "sha256Hash";
// interior mutability is not a concern here, the value is never modified
#[allow(clippy::declare_interior_mutable_const)]
const ACCEPTED_ENCODINGS: HeaderValue = HeaderValue::from_static("gzip, br, deflate");

enum APQError {
    PersistedQueryNotSupported,
    PersistedQueryNotFound,
    Other,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Compression {
    /// gzip
    Gzip,
    /// deflate
    Deflate,
    /// brotli
    Br,
}

impl Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compression::Gzip => write!(f, "gzip"),
            Compression::Deflate => write!(f, "deflate"),
            Compression::Br => write!(f, "br"),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
struct SubscriptionExtension {
    subscription_id: String,
    callback_url: url::Url,
    verifier: String,
}

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub(crate) struct SubgraphService {
    // Note: We use hyper::Client here in preference to reqwest to avoid expensive URL translation
    // in the hot path. We use reqwest elsewhere because it's convenient and some of the
    // opentelemetry crate require reqwest clients to work correctly (at time of writing).
    client: Decompression<hyper::Client<HttpsConnector<HttpConnector>>>,
    service: Arc<String>,

    /// Whether apq is enabled in the router for subgraph calls
    /// This is enabled by default can be configured as
    /// subgraph:
    ///      apq: <bool>
    /// If a subgraph sends the error message PERSISTED_QUERY_NOT_SUPPORTED,
    /// apq is set to false
    apq: Arc<AtomicBool>,
    /// Subscription config if enabled
    subscription_config: Option<SubscriptionConfig>,
    notify: Notify<String, graphql::Response>,
}

impl SubgraphService {
    pub(crate) fn new(
        service: impl Into<String>,
        enable_apq: bool,
        tls_cert_store: Option<RootCertStore>,
        enable_http2: bool,
        subscription_config: Option<SubscriptionConfig>,
        notify: Notify<String, graphql::Response>,
    ) -> Self {
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);
        let tls_config = match tls_cert_store {
            None => rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Some(store) => rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(store)
                .with_no_client_auth(),
        };
        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1();

        let connector = if enable_http2 {
            builder.enable_http2().wrap_connector(http_connector)
        } else {
            builder.wrap_connector(http_connector)
        };

        Self {
            client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(hyper::Client::builder().build(connector)),
            service: Arc::new(service.into()),
            apq: Arc::new(<AtomicBool>::new(enable_apq)),
            subscription_config,
            notify,
        }
    }
}

impl tower::Service<SubgraphRequest> for SubgraphService {
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let subscription_config = (request.operation_kind == OperationKind::Subscription)
            .then(|| self.subscription_config.clone())
            .flatten();
        let service_name = (*self.service).to_owned();

        // Do it only for subscription to dedup them
        let hashed_request = if request.operation_kind == OperationKind::Subscription {
            let subscription_config = match &subscription_config {
                Some(sub_cfg) => sub_cfg,
                None => {
                    return Box::pin(async move {
                        Err(BoxError::from(FetchError::SubrequestWsError {
                            service: service_name,
                            reason: "subscription is not enabled".to_string(),
                        }))
                    });
                }
            };
            if subscription_config.enable_deduplication {
                request.to_sha256()
            } else {
                Uuid::new_v4().to_string()
            }
        } else {
            String::new()
        };

        let SubgraphRequest {
            subgraph_request,
            context,
            ..
        } = request.clone();

        let (_, mut body) = subgraph_request.into_parts();

        let clone = self.client.clone();
        let client = std::mem::replace(&mut self.client, clone);

        let arc_apq_enabled = self.apq.clone();

        let mut notify = self.notify.clone();
        let make_calls = async move {
            // Subscription handling
            if request.operation_kind == OperationKind::Subscription
                && request.subscription_stream.is_some()
            {
                let subscription_config =
                    subscription_config.ok_or_else(|| FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: "subscription is not enabled".to_string(),
                        status_code: None,
                    })?;
                let mode = subscription_config.mode.get_subgraph_config(&service_name);

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
                        .await;
                    }
                    Some(SubscriptionMode::Callback(CallbackMode { public_url, .. })) => {
                        // Hash the subgraph_request
                        let subscription_id = hashed_request;

                        // Call create_or_subscribe on notify
                        let (handle, created) = notify
                            .create_or_subscribe(subscription_id.clone(), true)
                            .await?;

                        // If it existed before just send the right stream (handle) and early return
                        let mut stream_tx =
                            request.subscription_stream.clone().ok_or_else(|| {
                                FetchError::SubrequestWsError {
                                    service: service_name.clone(),
                                    reason: "cannot get the websocket stream".to_string(),
                                }
                            })?;
                        stream_tx.send(handle.into_stream()).await?;

                        if !created {
                            tracing::info!(
                                monotonic_counter.apollo_router_deduplicated_subscriptions_total = 1u64,
                                mode = %"callback",
                            );
                            // Dedup happens here
                            return Ok(SubgraphResponse::builder()
                                .context(context)
                                .extensions(Object::default())
                                .build());
                        }

                        // If not then put the subscription_id in the extensions for callback mode and continue
                        // Do this if the topic doesn't already exist
                        let callback_url =
                            public_url.join(&format!("/callback/{subscription_id}"))?;
                        // Generate verifier
                        let verifier = create_verifier(&subscription_id).map_err(|err| {
                            FetchError::SubrequestHttpError {
                                service: service_name.clone(),
                                reason: format!("{err:?}"),
                                status_code: None,
                            }
                        })?;

                        let subscription_extension = SubscriptionExtension {
                            subscription_id,
                            callback_url,
                            verifier,
                        };
                        body.extensions.insert(
                            "subscription",
                            serde_json_bytes::to_value(subscription_extension).map_err(|_err| {
                                FetchError::SubrequestHttpError {
                                    service: service_name.clone(),
                                    reason: String::from(
                                        "cannot serialize the subscription extension",
                                    ),
                                    status_code: None,
                                }
                            })?,
                        );
                    }
                    _ => {
                        return Err(Box::new(FetchError::SubrequestWsError {
                            service: service_name.clone(),
                            reason: "subscription mode is not enabled".to_string(),
                        }));
                    }
                }
            }

            // If APQ is not enabled, simply make the graphql call
            // with the same request body.
            let apq_enabled = arc_apq_enabled.as_ref();
            if !apq_enabled.load(Relaxed) {
                return call_http(request, body, context, client, service_name).await;
            }

            // Else, if APQ is enabled,
            // Calculate the query hash and try the request with
            // a persistedQuery instead of the whole query.
            let graphql::Request {
                query,
                operation_name,
                variables,
                extensions,
            } = body.clone();

            let hash_value = apq::calculate_hash_for_query(query.as_deref().unwrap_or_default());

            let persisted_query = serde_json_bytes::json!({
                HASH_VERSION_KEY: HASH_VERSION_VALUE,
                HASH_KEY: hash_value
            });

            let mut extensions_with_apq = extensions.clone();
            extensions_with_apq.insert(PERSISTED_QUERY_KEY, persisted_query);

            let mut apq_body = graphql::Request {
                query: None,
                operation_name,
                variables,
                extensions: extensions_with_apq,
            };

            let response = call_http(
                request.clone(),
                apq_body.clone(),
                context.clone(),
                client.clone(),
                service_name.clone(),
            )
            .await?;

            // Check the error for the request with only persistedQuery.
            // If PersistedQueryNotSupported, disable APQ for this subgraph
            // If PersistedQueryNotFound, add the original query to the request and retry.
            // Else, return the response like before.
            let gql_response = response.response.body();
            match get_apq_error(gql_response) {
                APQError::PersistedQueryNotSupported => {
                    apq_enabled.store(false, Relaxed);
                    call_http(request, body, context, client, service_name).await
                }
                APQError::PersistedQueryNotFound => {
                    apq_body.query = query;
                    call_http(request, apq_body, context, client, service_name).await
                }
                _ => Ok(response),
            }
        };

        Box::pin(make_calls)
    }
}

/// call websocket makes websocket calls with modified graphql::Request (body)
async fn call_websocket(
    mut notify: Notify<String, graphql::Response>,
    request: SubgraphRequest,
    context: Context,
    service_name: String,
    subgraph_cfg: &WebSocketConfiguration,
    subscription_hash: String,
) -> Result<SubgraphResponse, BoxError> {
    let SubgraphRequest {
        subgraph_request,
        subscription_stream,
        ..
    } = request;
    let mut subscription_stream_tx =
        subscription_stream.ok_or_else(|| FetchError::SubrequestWsError {
            service: service_name.clone(),
            reason: "cannot get the websocket stream".to_string(),
        })?;

    let (handle, created) = notify
        .create_or_subscribe(subscription_hash.clone(), false)
        .await?;
    if !created {
        subscription_stream_tx.send(handle.into_stream()).await?;
        tracing::info!(
            monotonic_counter.apollo_router_deduplicated_subscriptions_total = 1u64,
            mode = %"passthrough",
        );

        // Dedup happens here
        return Ok(SubgraphResponse::builder()
            .context(context)
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

    let request = get_websocket_request(service_name.clone(), parts, subgraph_cfg)?;
    let display_headers = context.contains_key(LOGGING_DISPLAY_HEADERS);
    let display_body = context.contains_key(LOGGING_DISPLAY_BODY);
    if display_headers {
        tracing::info!(http.request.headers = ?request.headers(), apollo.subgraph.name = %service_name, "Websocket request headers to subgraph {service_name:?}");
    }
    if display_body {
        tracing::info!(http.request.body = ?request.body(), apollo.subgraph.name = %service_name, "Websocket request body to subgraph {service_name:?}");
    }

    let (ws_stream, mut resp) = match request.uri().scheme_str() {
        Some("wss") => connect_async_tls_with_config(request, None, None).await,
        _ => connect_async(request).await,
    }
    .map_err(|err| FetchError::SubrequestWsError {
        service: service_name.clone(),
        reason: format!("cannot connect websocket to subgraph: {err}"),
    })?;

    if display_body {
        tracing::info!(
            response.body = %String::from_utf8_lossy(&resp.body_mut().take().unwrap_or_default()), apollo.subgraph.name = %service_name, "Raw response body from subgraph {service_name:?} received"
        );
    }

    let mut gql_stream = GraphqlWebSocket::new(
        convert_websocket_stream(ws_stream, subscription_hash.clone()),
        subscription_hash,
        subgraph_cfg.protocol,
        connection_params,
    )
    .await
    .map_err(|_| FetchError::SubrequestWsError {
        service: service_name.clone(),
        reason: "cannot get the GraphQL websocket stream".to_string(),
    })?;

    gql_stream
        .send(body)
        .await
        .map_err(|err| FetchError::SubrequestWsError {
            service: service_name,
            reason: format!("cannot send the subgraph request to websocket stream: {err:?}"),
        })?;
    let (mut gql_sink, gql_stream) = gql_stream.split();
    let (handle_sink, handle_stream) = handle.split();

    tokio::task::spawn(async move {
        let _ = gql_stream
            .map(Ok::<_, graphql::Error>)
            .forward(handle_sink)
            .await;

        if let Err(err) = gql_sink.close().await {
            tracing::trace!("cannot close the websocket stream: {err:?}");
        }
    });

    subscription_stream_tx.send(handle_stream).await?;

    Ok(SubgraphResponse::new_from_response(
        resp.map(|_| graphql::Response::default()),
        context,
    ))
}

/// call_http makes http calls with modified graphql::Request (body)
async fn call_http(
    request: SubgraphRequest,
    body: graphql::Request,
    context: Context,
    mut client: Decompression<Client<HttpsConnector<HttpConnector>>>,
    service_name: String,
) -> Result<SubgraphResponse, BoxError> {
    let SubgraphRequest {
        subgraph_request, ..
    } = request;

    let operation_name = subgraph_request
        .body()
        .operation_name
        .clone()
        .unwrap_or_default();
    let (parts, _) = subgraph_request.into_parts();

    let body = serde_json::to_string(&body).expect("JSON serialization should not fail");
    let compressed_body = compress(body, &parts.headers)
        .instrument(tracing::debug_span!("body_compression"))
        .await
        .map_err(|err| {
            tracing::error!(compress_error = format!("{err:?}").as_str());

            FetchError::CompressionError {
                service: service_name.clone(),
                reason: err.to_string(),
            }
        })?;

    let mut request = http::request::Request::from_parts(parts, compressed_body.into());
    let app_json: HeaderValue = HeaderValue::from_static(APPLICATION_JSON.essence_str());
    let app_graphql_json: HeaderValue =
        HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE);
    request.headers_mut().insert(CONTENT_TYPE, app_json.clone());
    request.headers_mut().insert(ACCEPT, app_json);
    request.headers_mut().append(ACCEPT, app_graphql_json);
    request
        .headers_mut()
        .insert(ACCEPT_ENCODING, ACCEPTED_ENCODINGS);

    let schema_uri = request.uri();
    let host = schema_uri.host().unwrap_or_default();
    let port = schema_uri.port_u16().unwrap_or_else(|| {
        let scheme = schema_uri.scheme_str();
        if scheme == Some("https") {
            443
        } else if scheme == Some("http") {
            80
        } else {
            0
        }
    });
    let display_headers = context.contains_key(LOGGING_DISPLAY_HEADERS);
    let display_body = context.contains_key(LOGGING_DISPLAY_BODY);
    if display_headers {
        tracing::info!(http.request.headers = ?request.headers(), apollo.subgraph.name = %service_name, "Request headers to subgraph {service_name:?}");
    }
    if display_body {
        tracing::info!(http.request.body = ?request.body(), apollo.subgraph.name = %service_name, "Request body to subgraph {service_name:?}");
    }

    let path = schema_uri.path();

    let subgraph_req_span = tracing::info_span!("subgraph_request",
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %schema_uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %service_name,
        "graphql.operation.name" = %operation_name,
    );
    get_text_map_propagator(|propagator| {
        propagator.inject_context(
            &subgraph_req_span.context(),
            &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
        );
    });
    let cloned_service_name = service_name.clone();
    let cloned_context = context.clone();
    let (parts, body) = async move {
        cloned_context.enter_active_request();
        let response = match client
            .call(request)
            .await {
                Err(err) => {
                    tracing::error!(fetch_error = format!("{err:?}").as_str());
                    cloned_context.leave_active_request();

                    return Err(FetchError::SubrequestHttpError {
                        status_code: None,
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }.into());
                }
                Ok(response) => response,
            };

        // Keep our parts, we'll need them later
        let (parts, body) = response.into_parts();
        if display_headers {
            tracing::info!(
                        http.response.headers = ?parts.headers, apollo.subgraph.name = %service_name, "Response headers from subgraph {service_name:?}"
                    );
        }
        if let Some(content_type) = parts.headers.get(header::CONTENT_TYPE) {
            if let Ok(content_type_str) = content_type.to_str() {
                // Using .contains because sometimes we could have charset included (example: "application/json; charset=utf-8")
                if !content_type_str.contains(APPLICATION_JSON.essence_str())
                    && !content_type_str.contains(GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                {
                    cloned_context.leave_active_request();

                    return if !parts.status.is_success() {

                        Err(BoxError::from(FetchError::SubrequestHttpError {
                            service: service_name.clone(),
                            status_code: Some(parts.status.as_u16()),
                            reason: format!(
                                "{}: {}",
                                parts.status.as_str(),
                                parts.status.canonical_reason().unwrap_or("Unknown")
                            ),
                        }))
                    } else {
                        Err(BoxError::from(FetchError::SubrequestHttpError {
                            status_code: Some(parts.status.as_u16()),
                            service: service_name.clone(),
                            reason: format!("subgraph didn't return JSON (expected content-type: {} or content-type: {GRAPHQL_JSON_RESPONSE_HEADER_VALUE}; found content-type: {content_type:?})", APPLICATION_JSON.essence_str()),
                        }))
                    };
                }
            }
        }

        let body = match hyper::body::to_bytes(body)
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await {
                Err(err) => {
                    cloned_context.leave_active_request();

                    tracing::error!(fetch_error = format!("{err:?}").as_str());

                return Err(FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.clone(),
                    reason: err.to_string(),
                }.into())

                }, Ok(body) => body,
            };

            cloned_context.leave_active_request();

        Ok((parts, body))
    }.instrument(subgraph_req_span).await?;

    if display_body {
        tracing::info!(
            http.response.body = %String::from_utf8_lossy(&body), apollo.subgraph.name = %cloned_service_name, "Raw response body from subgraph {cloned_service_name:?} received"
        );
    }

    let graphql: graphql::Response =
        tracing::debug_span!("parse_subgraph_response").in_scope(|| {
            graphql::Response::from_bytes(&cloned_service_name, body).map_err(|error| {
                FetchError::SubrequestMalformedResponse {
                    service: cloned_service_name.clone(),
                    reason: error.to_string(),
                }
            })
        })?;

    let resp = http::Response::from_parts(parts, graphql);

    Ok(SubgraphResponse::new_from_response(resp, context))
}

fn get_websocket_request(
    service_name: String,
    mut parts: http::request::Parts,
    subgraph_ws_cfg: &WebSocketConfiguration,
) -> Result<http::Request<()>, FetchError> {
    let mut subgraph_url = url::Url::parse(&parts.uri.to_string()).map_err(|err| {
        tracing::error!("cannot parse subgraph url {}: {err:?}", parts.uri);
        FetchError::SubrequestWsError {
            service: service_name.clone(),
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
            service: service_name.clone(),
            reason: "cannot set a scheme on websocket url".to_string(),
        }
    })?;

    let subgraph_url = match &subgraph_ws_cfg.path {
        Some(path) => subgraph_url
            .join(path)
            .map_err(|_| FetchError::SubrequestWsError {
                service: service_name.clone(),
                reason: "cannot parse subgraph url with the specific websocket path".to_string(),
            })?,
        None => subgraph_url,
    };
    let mut request = subgraph_url.into_client_request().map_err(|err| {
        tracing::error!("cannot create websocket client request: {err:?}");

        FetchError::SubrequestWsError {
            service: service_name.clone(),
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

fn get_apq_error(gql_response: &graphql::Response) -> APQError {
    for error in &gql_response.errors {
        // Check if error message is an APQ error
        match error.message.as_str() {
            PERSISTED_QUERY_NOT_FOUND_MESSAGE => {
                return APQError::PersistedQueryNotFound;
            }
            PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE => {
                return APQError::PersistedQueryNotSupported;
            }
            _ => {}
        }
        // Check if extensions contains the APQ error in "code"
        if let Some(value) = error.extensions.get(CODE_STRING) {
            if value == PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE {
                return APQError::PersistedQueryNotFound;
            } else if value == PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE {
                return APQError::PersistedQueryNotSupported;
            }
        }
    }
    APQError::Other
}

pub(crate) async fn compress(body: String, headers: &HeaderMap) -> Result<Vec<u8>, BoxError> {
    let content_encoding = headers.get(&CONTENT_ENCODING);
    match content_encoding {
        Some(content_encoding) => match content_encoding.to_str()? {
            "br" => {
                let mut br_encoder = BrotliEncoder::new(Vec::new());
                br_encoder.write_all(body.as_bytes()).await?;
                br_encoder.shutdown().await?;

                Ok(br_encoder.into_inner())
            }
            "gzip" => {
                let mut gzip_encoder = GzipEncoder::new(Vec::new());
                gzip_encoder.write_all(body.as_bytes()).await?;
                gzip_encoder.shutdown().await?;

                Ok(gzip_encoder.into_inner())
            }
            "deflate" => {
                let mut df_encoder = ZlibEncoder::new(Vec::new());
                df_encoder.write_all(body.as_bytes()).await?;
                df_encoder.shutdown().await?;

                Ok(df_encoder.into_inner())
            }
            "identity" => Ok(body.into_bytes()),
            unknown => {
                tracing::error!("unknown content-encoding value '{:?}'", unknown);
                Err(BoxError::from(format!(
                    "unknown content-encoding value '{unknown:?}'",
                )))
            }
        },
        None => Ok(body.into_bytes()),
    }
}

#[derive(Clone)]
pub(crate) struct SubgraphServiceFactory {
    pub(crate) services: Arc<HashMap<String, Arc<dyn MakeSubgraphService>>>,
    pub(crate) plugins: Arc<Plugins>,
}

impl SubgraphServiceFactory {
    pub(crate) fn new(
        services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
        plugins: Arc<Plugins>,
    ) -> Self {
        SubgraphServiceFactory {
            services: Arc::new(services.into_iter().collect()),
            plugins,
        }
    }

    pub(crate) fn create(
        &self,
        name: &str,
    ) -> Option<BoxService<SubgraphRequest, SubgraphResponse, BoxError>> {
        self.services.get(name).map(|service| {
            let service = service.make();
            self.plugins
                .iter()
                .rev()
                .fold(service, |acc, (_, e)| e.subgraph_service(name, acc))
        })
    }
}

/// make new instances of the subgraph service
///
/// there can be multiple instances of that service executing at any given time
pub(crate) trait MakeSubgraphService: Send + Sync + 'static {
    fn make(&self) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError>;
}

impl<S> MakeSubgraphService for S
where
    S: Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <S as Service<SubgraphRequest>>::Future: Send,
{
    fn make(&self) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        self.clone().boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use axum::extract::ws::Message;
    use axum::extract::ConnectInfo;
    use axum::extract::WebSocketUpgrade;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use axum::Server;
    use bytes::Buf;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use http::header::HOST;
    use http::StatusCode;
    use http::Uri;
    use hyper::service::make_service_fn;
    use hyper::Body;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::service_fn;
    use tower::ServiceExt;
    use SubgraphRequest;

    use super::*;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::plugins::subscription::SubgraphPassthroughMode;
    use crate::plugins::subscription::SubscriptionModeConfig;
    use crate::protocols::websocket::ClientMessage;
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::Context;

    // starts a local server emulating a subgraph returning status code 400
    async fn emulate_subgraph_bad_request(socket_addr: SocketAddr) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::BAD_REQUEST)
                .body(
                    serde_json::to_string(&Response {
                        errors: vec![Error::builder()
                            .message("This went wrong")
                            .extension_code("FETCH_ERROR")
                            .build()],
                        ..Response::default()
                    })
                    .expect("always valid")
                    .into(),
                )
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning status code 401
    async fn emulate_subgraph_unauthorized(socket_addr: SocketAddr) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "text/html")
                .status(StatusCode::UNAUTHORIZED)
                .body(r#""#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_bad_response_format(socket_addr: SocketAddr) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "text/html")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning compressed response
    async fn emulate_subgraph_compressed_response(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            // Check the compression of the body
            let mut encoder = GzipEncoder::new(Vec::new());
            encoder
                .write_all(
                    &serde_json::to_vec(&Request::builder().query("query".to_string()).build())
                        .unwrap(),
                )
                .await
                .unwrap();
            encoder.shutdown().await.unwrap();
            let compressed_body = encoder.into_inner();
            assert_eq!(
                compressed_body,
                hyper::body::to_bytes(request.into_body())
                    .await
                    .unwrap()
                    .to_vec()
            );

            let original_body = Response {
                data: Some(Value::String(ByteString::from("test"))),
                ..Response::default()
            };
            let mut encoder = GzipEncoder::new(Vec::new());
            encoder
                .write_all(&serde_json::to_vec(&original_body).unwrap())
                .await
                .unwrap();
            encoder.shutdown().await.unwrap();
            let compressed_body = encoder.into_inner();

            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header(CONTENT_ENCODING, "gzip")
                .status(StatusCode::OK)
                .body(compressed_body.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotSupported",...],...}
    async fn emulate_persisted_query_not_supported_message(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");
            match graphql_request {
                Ok(request) => {
                    if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![Error::builder()
                                        .message(PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE)
                                        .extension_code("Random code")
                                        .build()],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }

                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_SUPPORTED"}],...}
    async fn emulate_persisted_query_not_supported_extension_code(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");
            match graphql_request {
                Ok(request) => {
                    if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![Error::builder()
                                        .message("Random message")
                                        .extension_code(
                                            PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE,
                                        )
                                        .build()],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }

                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotFound",...],...}
    async fn emulate_persisted_query_not_found_message(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!("Recieved request without persisted query in persisted_query_not_found test.")
                    }

                    if request.query.is_none() {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![Error::builder()
                                        .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                                        .extension_code("Random Code")
                                        .build()],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    } else {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}],...}
    async fn emulate_persisted_query_not_found_extension_code(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!("Recieved request without persisted query in persisted_query_not_found test.")
                    }

                    if request.query.is_none() {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![Error::builder()
                                        .message("Random message")
                                        .extension_code(PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE)
                                        .build()],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    } else {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request with apq
    // and panics if it does not find a persistedQuery.
    async fn emulate_expected_apq_enabled_configuration(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!("persistedQuery expected when configuration has apq_enabled=true")
                    }

                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request without apq
    // and panics if it finds a persistedQuery.
    async fn emulate_expected_apq_disabled_configuration(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!(
                            "persistedQuery not expected when configuration has apq_enabled=false"
                        )
                    }

                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        server.await.unwrap();
    }

    async fn emulate_correct_websocket_server(socket_addr: SocketAddr) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            let res = ws.on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::Text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
                assert!(matches!(subscribe_msg, ClientMessage::Subscribe { .. }));
                let client_id = if let ClientMessage::Subscribe { payload, id } = subscribe_msg {
                    assert_eq!(
                        payload,
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build()
                    );

                    id
                } else {
                    panic!("subscribe message should be sent");
                };

                socket
                    .send(Message::Text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id, payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = Server::bind(&socket_addr)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>());
        server.await.unwrap();
    }

    async fn emulate_incorrect_websocket_server(socket_addr: SocketAddr) {
        async fn ws_handler(
            _ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            Ok((http::StatusCode::BAD_REQUEST, "bad request"))
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = Server::bind(&socket_addr)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>());
        server.await.unwrap();
    }

    fn subscription_config() -> SubscriptionConfig {
        SubscriptionConfig {
            mode: SubscriptionModeConfig {
                callback: None,
                passthrough: Some(SubgraphPassthroughMode {
                    all: None,
                    subgraphs: [(
                        "test".to_string(),
                        WebSocketConfiguration {
                            path: Some(String::from("/ws")),
                            protocol: WebSocketProtocol::default(),
                        },
                    )]
                    .into(),
                }),
            },
            enable_deduplication: true,
            max_opened_subscriptions: None,
            queue_capacity: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2222").unwrap();
        let spawned_task = tokio::task::spawn(emulate_correct_websocket_server(socket_addr));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            None,
            false,
            subscription_config().into(),
            Notify::builder().build(),
        );
        let (tx, mut rx) = mpsc::channel(2);

        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(
                            Request::builder()
                                .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                                .build(),
                        )
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build(),
                    )
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Subscription,
                context: Context::new(),
                subscription_stream: Some(tx),
                connection_closed_signal: None,
            })
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx.next().await.unwrap();
        let message = gql_stream.next().await.unwrap();
        assert_eq!(
            message,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );
        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket_with_error() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2323").unwrap();
        tokio::task::spawn(emulate_incorrect_websocket_server(socket_addr));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            None,
            false,
            subscription_config().into(),
            Notify::builder().build(),
        );
        let (tx, _rx) = mpsc::channel(2);

        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
        let err = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(
                            Request::builder()
                                .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                                .build(),
                        )
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build(),
                    )
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Subscription,
                context: Context::new(),
                subscription_stream: Some(tx),
                connection_closed_signal: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Websocket fetch failed from 'test': cannot connect websocket to subgraph: HTTP error: 400 Bad Request".to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code_should_not_fail() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2626").unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "This went wrong"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_content_type() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2525").unwrap();
        tokio::task::spawn(emulate_subgraph_bad_response_format(socket_addr));

        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let err = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "HTTP fetch failed from 'test': subgraph didn't return JSON (expected content-type: application/json or content-type: application/graphql-response+json; found content-type: \"text/html\")"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_compressed_request_response_body() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2727").unwrap();
        tokio::task::spawn(emulate_subgraph_compressed_response(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", false, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query".to_string()).build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .header(CONTENT_ENCODING, "gzip")
                    .uri(url)
                    .body(Request::builder().query("query".to_string()).build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();
        // Test the right decompression of the body
        let resp_from_subgraph = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &resp_from_subgraph);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unauthorized() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2828").unwrap();
        tokio::task::spawn(emulate_subgraph_unauthorized(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let err = subgraph_service
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_supported_message() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2929").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_message(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
        assert!(!subgraph_service.apq.as_ref().load(Relaxed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_supported_extension_code() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3030").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_extension_code(
            socket_addr,
        ));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
        assert!(!subgraph_service.apq.as_ref().load(Relaxed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_found_message() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3131").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_message(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_found_extension_code() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3232").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_extension_code(
            socket_addr,
        ));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_apq_enabled_subgraph_configuration() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3333").unwrap();
        tokio::task::spawn(emulate_expected_apq_enabled_configuration(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", true, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_apq_disabled_subgraph_configuration() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3434").unwrap();
        tokio::task::spawn(emulate_expected_apq_disabled_configuration(socket_addr));
        let subgraph_service =
            SubgraphService::new("test", false, None, true, None, Notify::default());

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(SubgraphRequest {
                supergraph_request: Arc::new(
                    http::Request::builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .body(Request::builder().query("query").build())
                        .expect("expecting valid request"),
                ),
                subgraph_request: http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(url)
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
                subscription_stream: None,
                connection_closed_signal: None,
            })
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }
}
