//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use ::serde::Deserialize;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZlibEncoder;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::SinkExt;
use futures::StreamExt;
use futures::TryFutureExt;
use global::get_text_map_propagator;
use http::header::ACCEPT;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::{self};
use http::response::Parts;
use http::HeaderMap;
use http::HeaderValue;
use http::Request;
use hyper::client::HttpConnector;
use hyper::Body;
use hyper::Client;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use mediatype::names::APPLICATION;
use mediatype::names::JSON;
use mediatype::MediaType;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use rustls::ClientConfig;
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

use super::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use super::Plugins;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::subscription::create_verifier;
use crate::plugins::subscription::CallbackMode;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::SubscriptionMode;
use crate::plugins::subscription::WebSocketConfiguration;
use crate::plugins::subscription::SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;
use crate::plugins::traffic_shaping::Http2Config;
use crate::protocols::websocket::convert_websocket_stream;
use crate::protocols::websocket::GraphqlWebSocket;
use crate::query_planner::OperationKind;
use crate::services::layers::apq;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Configuration;
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
const GRAPHQL_RESPONSE: mediatype::Name = mediatype::Name::new_unchecked("graphql-response");
const POOL_IDLE_TIMEOUT_DURATION: Option<Duration> = Some(Duration::from_secs(5));

// interior mutability is not a concern here, the value is never modified
#[allow(clippy::declare_interior_mutable_const)]
static ACCEPTED_ENCODINGS: HeaderValue = HeaderValue::from_static("gzip, br, deflate");
pub(crate) static APPLICATION_JSON_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("application/json");
static APP_GRAPHQL_JSON: HeaderValue = HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE);

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

#[cfg_attr(test, derive(Deserialize))]
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
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &Configuration,
        tls_root_store: &Option<RootCertStore>,
        http2: Http2Config,
        subscription_config: Option<SubscriptionConfig>,
    ) -> Result<Self, BoxError> {
        let name: String = service.into();
        let tls_cert_store = configuration
            .tls
            .subgraph
            .subgraphs
            .get(&name)
            .as_ref()
            .and_then(|subgraph| subgraph.create_certificate_store())
            .transpose()?
            .or_else(|| tls_root_store.clone());
        let enable_apq = configuration
            .apq
            .subgraph
            .subgraphs
            .get(&name)
            .map(|apq| apq.enabled)
            .unwrap_or(configuration.apq.subgraph.all.enabled);
        let client_cert_config = configuration
            .tls
            .subgraph
            .subgraphs
            .get(&name)
            .as_ref()
            .and_then(|tls| tls.client_authentication.as_ref())
            .or(configuration
                .tls
                .subgraph
                .all
                .client_authentication
                .as_ref());

        let tls_builder = rustls::ClientConfig::builder().with_safe_defaults();
        let tls_client_config = match (tls_cert_store, client_cert_config) {
            (None, None) => tls_builder.with_native_roots().with_no_client_auth(),
            (Some(store), None) => tls_builder
                .with_root_certificates(store)
                .with_no_client_auth(),
            (None, Some(client_auth_config)) => {
                tls_builder.with_native_roots().with_client_auth_cert(
                    client_auth_config.certificate_chain.clone(),
                    client_auth_config.key.clone(),
                )?
            }
            (Some(store), Some(client_auth_config)) => tls_builder
                .with_root_certificates(store)
                .with_client_auth_cert(
                    client_auth_config.certificate_chain.clone(),
                    client_auth_config.key.clone(),
                )?,
        };

        Ok(SubgraphService::new(
            name,
            enable_apq,
            http2,
            subscription_config,
            tls_client_config,
            configuration.notify.clone(),
        ))
    }

    pub(crate) fn new(
        service: impl Into<String>,
        enable_apq: bool,
        http2: Http2Config,
        subscription_config: Option<SubscriptionConfig>,
        tls_config: ClientConfig,
        notify: Notify<String, graphql::Response>,
    ) -> Self {
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1();

        let connector = if http2 != Http2Config::Disable {
            builder.enable_http2().wrap_connector(http_connector)
        } else {
            builder.wrap_connector(http_connector)
        };

        let http_client = hyper::Client::builder()
            .pool_idle_timeout(POOL_IDLE_TIMEOUT_DURATION)
            .http2_only(http2 == Http2Config::Http2Only)
            .build(connector);
        Self {
            client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(http_client),
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
                    Some(SubscriptionMode::Callback(CallbackMode {
                        public_url, path, ..
                    })) => {
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
                                    reason: "cannot get the callback stream".to_string(),
                                }
                            })?;
                        stream_tx.send(handle.into_stream()).await?;
                        tracing::info!(
                            monotonic_counter.apollo.router.operations.subscriptions = 1u64,
                            subscriptions.mode = %"callback",
                            subscriptions.deduplicated = !created,
                            subgraph.service.name = service_name,
                        );
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
                        let callback_url = public_url.join(&format!(
                            "{}/{subscription_id}",
                            path.as_deref().unwrap_or("/callback")
                        ))?;
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
                return call_http(request, body, context, client, &service_name).await;
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
                &service_name,
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
                    call_http(request, body, context, client, &service_name).await
                }
                APQError::PersistedQueryNotFound => {
                    apq_body.query = query;
                    call_http(request, apq_body, context, client, &service_name).await
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
    let operation_name = request
        .subgraph_request
        .body()
        .operation_name
        .clone()
        .unwrap_or_default();

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
    tracing::info!(
        monotonic_counter.apollo.router.operations.subscriptions = 1u64,
        subscriptions.mode = %"passthrough",
        subscriptions.deduplicated = !created,
        subgraph.service.name = service_name,
    );
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

    let signing_params = context
        .private_entries
        .lock()
        .get::<SigningParamsConfig>()
        .cloned();

    let request = if let Some(signing_params) = signing_params {
        signing_params
            .sign_empty(request, service_name.as_str())
            .await?
    } else {
        request
    };

    if display_headers {
        tracing::info!(http.request.headers = ?request.headers(), apollo.subgraph.name = %service_name, "Websocket request headers to subgraph {service_name:?}");
    }

    if display_body {
        tracing::info!(http.request.body = ?request.body(), apollo.subgraph.name = %service_name, "Websocket request body to subgraph {service_name:?}");
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

    let subgraph_req_span = tracing::info_span!("subgraph_request",
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %service_name,
        "graphql.operation.name" = %operation_name,
    );

    let (ws_stream, mut resp) = match request.uri().scheme_str() {
        Some("wss") => {
            connect_async_tls_with_config(request, None, false, None)
                .instrument(subgraph_req_span)
                .await
        }
        _ => connect_async(request).instrument(subgraph_req_span).await,
    }
    .map_err(|err| {
        if display_body || display_headers {
            tracing::info!(
                http.response.error = format!("{:?}", &err), apollo.subgraph.name = %service_name, "Websocket connection error from subgraph {service_name:?} received"
            );
        }
        FetchError::SubrequestWsError {
            service: service_name.clone(),
            reason: format!("cannot connect websocket to subgraph: {err}"),
        }
    })?;

    if display_headers {
        tracing::info!(response.headers = ?resp.headers(), apollo.subgraph.name = %service_name, "Websocket response headers to subgraph {service_name:?}");
    }
    if display_body {
        tracing::info!(
            response.body = %String::from_utf8_lossy(&resp.body_mut().take().unwrap_or_default()), apollo.subgraph.name = %service_name, "Websocket response body from subgraph {service_name:?} received"
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
    client: Decompression<Client<HttpsConnector<HttpConnector>>>,
    service_name: &str,
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
                service: service_name.to_string(),
                reason: err.to_string(),
            }
        })?;

    let mut request = http::request::Request::from_parts(parts, compressed_body.into());

    request
        .headers_mut()
        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
    request
        .headers_mut()
        .insert(ACCEPT, APPLICATION_JSON_HEADER_VALUE.clone());
    request
        .headers_mut()
        .append(ACCEPT, APP_GRAPHQL_JSON.clone());
    request
        .headers_mut()
        .insert(ACCEPT_ENCODING, ACCEPTED_ENCODINGS.clone());

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

    // The graphql spec is lax about what strategy to use for processing responses: https://github.com/graphql/graphql-over-http/blob/main/spec/GraphQLOverHTTP.md#processing-the-response
    //
    // "If the response uses a non-200 status code and the media type of the response payload is application/json
    // then the client MUST NOT rely on the body to be a well-formed GraphQL response since the source of the response
    // may not be the server but instead some intermediary such as API gateways, proxies, firewalls, etc."
    //
    // The TLDR of this is that it's really asking us to do the best we can with whatever information we have with some modifications depending on content type.
    // Our goal is to give the user the most relevant information possible in the response errors
    //
    // Rules:
    // 1. If the content type of the response is not `application/json` or `application/graphql-response+json` then we won't try to parse.
    // 2. If an HTTP status is not 2xx it will always be attached as a graphql error.
    // 3. If the response type is `application/json` and status is not 2xx and the body the entire body will be output if the response is not valid graphql.

    let display_headers = context.contains_key(LOGGING_DISPLAY_HEADERS);
    let display_body = context.contains_key(LOGGING_DISPLAY_BODY);

    let signing_params = context
        .private_entries
        .lock()
        .get::<SigningParamsConfig>()
        .cloned();

    let request = if let Some(signing_params) = signing_params {
        signing_params.sign(request, service_name).await?
    } else {
        request
    };

    // Print out the debug for the request
    if display_headers {
        tracing::info!(http.request.headers = ?request.headers(), apollo.subgraph.name = %service_name, "Request headers to subgraph {service_name:?}");
    }
    if display_body {
        tracing::info!(http.request.body = ?request.body(), apollo.subgraph.name = %service_name, "Request body to subgraph {service_name:?}");
    }

    // Perform the actual fetch. If this fails then we didn't manage to make the call at all, so we can't do anything with it.
    let (parts, content_type, body) = do_fetch(
        client,
        &context,
        service_name,
        request,
        display_headers,
        display_body,
    )
    .instrument(subgraph_req_span)
    .await?;

    // Print out the debug for the response
    if display_headers {
        tracing::info!(response.headers = ?parts.headers, apollo.subgraph.name = %service_name, "Response headers from subgraph {service_name:?}");
    }
    if display_body {
        if let Some(Ok(b)) = &body {
            tracing::info!(
                response.body = %String::from_utf8_lossy(b), apollo.subgraph.name = %service_name, "Raw response body from subgraph {service_name:?} received"
            );
        }
    }

    let mut graphql_response = match (content_type, body, parts.status.is_success()) {
        (Ok(ContentType::ApplicationGraphqlResponseJson), Some(Ok(body)), _)
        | (Ok(ContentType::ApplicationJson), Some(Ok(body)), true) => {
            // Application graphql json expects valid graphql response
            // Application json expects valid graphql response if 2xx
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                graphql::Response::from_bytes(service_name, body).unwrap_or_else(|error| {
                    graphql::Response::builder()
                        .error(error.to_graphql_error(None))
                        .build()
                })
            })
        }
        (Ok(ContentType::ApplicationJson), Some(Ok(body)), false) => {
            // Application json does not expect a valid graphql response if not 2xx.
            // If parse fails then attach the entire payload as an error
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                let mut original_response = String::from_utf8_lossy(&body).to_string();
                if original_response.is_empty() {
                    original_response = "<empty response body>".into()
                }
                graphql::Response::from_bytes(service_name, body).unwrap_or_else(|_error| {
                    graphql::Response::builder()
                        .error(
                            FetchError::SubrequestMalformedResponse {
                                service: service_name.to_string(),
                                reason: original_response,
                            }
                            .to_graphql_error(None),
                        )
                        .build()
                })
            })
        }
        (content_type, body, _) => {
            // Something went wrong, compose a response with errors if they are present
            let mut graphql_response = graphql::Response::builder().build();
            if let Err(err) = content_type {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            if let Some(Err(err)) = body {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            graphql_response
        }
    };

    // Add an error for response codes that are not 2xx
    if !parts.status.is_success() {
        let status = parts.status;
        graphql_response.errors.insert(
            0,
            FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                status_code: Some(status.as_u16()),
                reason: format!(
                    "{}: {}",
                    status.as_str(),
                    status.canonical_reason().unwrap_or("Unknown")
                ),
            }
            .to_graphql_error(None),
        )
    }

    let resp = http::Response::from_parts(parts, graphql_response);
    Ok(SubgraphResponse::new_from_response(resp, context))
}

enum ContentType {
    ApplicationJson,
    ApplicationGraphqlResponseJson,
}

fn get_graphql_content_type(service_name: &str, parts: &Parts) -> Result<ContentType, FetchError> {
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().map(MediaType::parse));
    match content_type {
        Some(Ok(Ok(content_type))) => {
            if content_type.ty == APPLICATION && content_type.subty == JSON {
                Ok(ContentType::ApplicationJson)
            } else if content_type.ty == APPLICATION
                && content_type.subty == GRAPHQL_RESPONSE
                && content_type.suffix == Some(JSON)
            {
                Ok(ContentType::ApplicationGraphqlResponseJson)
            } else {
                Err(FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: service_name.to_string(),
                    reason: format!("subgraph didn't return JSON (expected content-type: {} or content-type: {}; found content-type: {content_type})", APPLICATION_JSON.essence_str(), GRAPHQL_JSON_RESPONSE_HEADER_VALUE),
                })
            }
        }
        None | Some(_) => Err(FetchError::SubrequestHttpError {
            status_code: Some(parts.status.as_u16()),
            service: service_name.to_string(),
            reason: format!(
                "subgraph didn't return JSON (expected content-type: {} or content-type: {})",
                APPLICATION_JSON.essence_str(),
                GRAPHQL_JSON_RESPONSE_HEADER_VALUE
            ),
        }),
    }
}

async fn do_fetch(
    mut client: Decompression<Client<HttpsConnector<HttpConnector>>>,
    context: &Context,
    service_name: &str,
    request: Request<Body>,
    display_headers: bool,
    display_body: bool,
) -> Result<
    (
        Parts,
        Result<ContentType, FetchError>,
        Option<Result<Bytes, FetchError>>,
    ),
    FetchError,
> {
    let _active_request_guard = context.enter_active_request();
    let response = client
        .call(request)
        .map_err(|err| {
            tracing::error!(fetch_error = ?err);
            FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.to_string(),
                reason: err.to_string(),
            }
        })
        .await?;

    let (parts, body) = response.into_parts();
    // Print out debug for the response
    if display_headers {
        tracing::info!(
            http.response.headers = ?parts.headers, apollo.subgraph.name = %service_name, "Response headers from subgraph {service_name:?}"
        );
    }
    let content_type = get_graphql_content_type(service_name, &parts);

    let body = if content_type.is_ok() {
        let body = hyper::body::to_bytes(body)
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = ?err);
                FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: service_name.to_string(),
                    reason: err.to_string(),
                }
            });
        if let Ok(body) = &body {
            if display_body {
                tracing::info!(
                    http.response.body = %String::from_utf8_lossy(body), apollo.subgraph.name = %service_name, "Raw response body from subgraph {service_name:?} received"
                );
            }
        }
        Some(body)
    } else {
        None
    };
    Ok((parts, content_type, body))
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
    use std::io;
    use std::net::SocketAddr;
    use std::net::TcpListener;
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
    use http::Version;
    use hyper::server::conn::AddrIncoming;
    use hyper::service::make_service_fn;
    use hyper::Body;
    use hyper_rustls::TlsAcceptor;
    use rustls::server::AllowAnyAuthenticatedClient;
    use rustls::Certificate;
    use rustls::PrivateKey;
    use rustls::ServerConfig;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::service_fn;
    use tower::ServiceExt;
    use url::Url;
    use SubgraphRequest;

    use super::*;
    use crate::configuration::load_certs;
    use crate::configuration::load_key;
    use crate::configuration::TlsClientAuth;
    use crate::configuration::TlsSubgraph;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::plugins::subscription::SubgraphPassthroughMode;
    use crate::plugins::subscription::SubscriptionModeConfig;
    use crate::plugins::subscription::SUBSCRIPTION_CALLBACK_HMAC_KEY;
    use crate::protocols::websocket::ClientMessage;
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::Context;

    // starts a local server emulating a subgraph returning status code 400
    async fn emulate_subgraph_bad_request(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning status code 401
    async fn emulate_subgraph_unauthorized(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::UNAUTHORIZED)
                .body(r#""#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_ok_status_invalid_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_invalid_response_invalid_status_application_json(
        listener: TcpListener,
    ) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::UNAUTHORIZED)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_invalid_response_invalid_status_application_graphql(
        listener: TcpListener,
    ) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                .status(StatusCode::UNAUTHORIZED)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_application_json_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(r#"{"data": null}"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_application_graphql_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                .status(StatusCode::OK)
                .body(r#"{"data": null}"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_bad_response_format(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "text/html")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning compressed response
    async fn emulate_subgraph_compressed_response(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotSupported",...],...}
    async fn emulate_persisted_query_not_supported_message(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_SUPPORTED"}],...}
    async fn emulate_persisted_query_not_supported_extension_code(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotFound",...],...}
    async fn emulate_persisted_query_not_found_message(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}],...}
    async fn emulate_persisted_query_not_found_extension_code(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request with apq
    // and panics if it does not find a persistedQuery.
    async fn emulate_expected_apq_enabled_configuration(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request without apq
    // and panics if it finds a persistedQuery.
    async fn emulate_expected_apq_disabled_configuration(listener: TcpListener) {
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
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    async fn emulate_correct_websocket_server(listener: TcpListener) {
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
        let server = Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service_with_connect_info::<SocketAddr>());
        server.await.unwrap();
    }

    async fn emulate_incorrect_websocket_server(listener: TcpListener) {
        async fn ws_handler(
            _ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            Ok((http::StatusCode::BAD_REQUEST, "bad request"))
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service_with_connect_info::<SocketAddr>());
        server.await.unwrap();
    }

    async fn emulate_subgraph_with_callback_data(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");
            let graphql_request = graphql_request.unwrap();
            assert!(graphql_request.extensions.contains_key("subscription"));
            let subscription_extension: SubscriptionExtension = serde_json_bytes::from_value(
                graphql_request
                    .extensions
                    .get("subscription")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
            assert_eq!(
                subscription_extension.callback_url.to_string(),
                format!(
                    "http://localhost:4000/testcallback/{}",
                    subscription_extension.subscription_id
                )
            );

            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response::builder().data(Value::Null).build())
                        .expect("always valid")
                        .into(),
                )
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener).unwrap().serve(make_svc);
        server.await.unwrap();
    }

    fn subscription_config() -> SubscriptionConfig {
        SubscriptionConfig {
            enabled: true,
            mode: SubscriptionModeConfig {
                callback: Some(CallbackMode {
                    public_url: Url::parse("http://localhost:4000").unwrap(),
                    listen: None,
                    path: Some("/testcallback".to_string()),
                    subgraphs: vec![String::from("testbis")].into_iter().collect(),
                }),
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
    async fn test_subgraph_service_callback() {
        let _ = SUBSCRIPTION_CALLBACK_HMAC_KEY.set(String::from("TESTEST"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_subgraph_with_callback_data(listener));
        let subgraph_service = SubgraphService::new(
            "testbis",
            true,
            Http2Config::Disable,
            subscription_config().into(),
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::builder().build(),
        );
        let (tx, _rx) = mpsc::channel(2);
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
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
        response.response.body().errors.iter().for_each(|e| {
            println!("error: {}", e.message);
        });
        assert!(response.response.body().errors.is_empty());
        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_content_type_application_graphql() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_graphql_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_content_type_application_json() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_json_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_invalid_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_ok_status_invalid_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "service 'test' response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_json() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_json(listener),
        );
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "service 'test' response was malformed: invalid"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_graphql() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_graphql(listener),
        );
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "service 'test' response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_correct_websocket_server(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Disable,
            subscription_config().into(),
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_incorrect_websocket_server(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Disable,
            subscription_config().into(),
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "HTTP fetch failed from 'test': 400: Bad Request"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "This went wrong"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_content_type() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_response_format(listener));

        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "HTTP fetch failed from 'test': subgraph didn't return JSON (expected content-type: application/json or content-type: application/graphql-response+json; found content-type: text/html)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_compressed_request_response_body() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_compressed_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            false,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unauthorized(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_supported_message() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_message(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_extension_code(
            listener,
        ));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_message(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_extension_code(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_expected_apq_enabled_configuration(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_expected_apq_disabled_configuration(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            false,
            Http2Config::Enable,
            None,
            ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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

    async fn tls_server(
        listener: tokio::net::TcpListener,
        certificates: Vec<Certificate>,
        key: PrivateKey,
        body: &'static str,
    ) {
        let acceptor = TlsAcceptor::builder()
            .with_single_cert(certificates, key)
            .unwrap()
            .with_all_versions_alpn()
            .with_incoming(AddrIncoming::from_listener(listener).unwrap());
        let service = make_service_fn(|_| async {
            Ok::<_, io::Error>(service_fn(|_req| async {
                Ok::<_, io::Error>(
                    http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .version(Version::HTTP_11)
                        .body::<Body>(body.into())
                        .unwrap(),
                )
            }))
        });
        let server = Server::builder(acceptor).serve(service);
        server.await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tls_self_signed() {
        let certificate_pem = include_str!("./testdata/server_self_signed.crt");
        let key_pem = include_str!("./testdata/server.key");

        let certificates = load_certs(certificate_pem).unwrap();
        let key = load_key(key_pem).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(tls_server(listener, certificates, key, r#"{"data": null}"#));

        // we cannot parse a configuration from text, because certificates are generally
        // added by file expansion and we don't have access to that here, and inserting
        // the PEM data directly generates parsing issues due to end of line characters
        let mut config = Configuration::default();
        config.tls.subgraph.subgraphs.insert(
            "test".to_string(),
            TlsSubgraph {
                certificate_authorities: Some(certificate_pem.into()),
                client_authentication: None,
            },
        );
        let subgraph_service =
            SubgraphService::from_config("test", &config, &None, Http2Config::Enable, None)
                .unwrap();

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
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

        assert_eq!(response.response.body().data, Some(Value::Null));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tls_custom_root() {
        let certificate_pem = include_str!("./testdata/server.crt");
        let ca_pem = include_str!("./testdata/CA/ca.crt");
        let key_pem = include_str!("./testdata/server.key");

        let mut certificates = load_certs(certificate_pem).unwrap();
        certificates.extend(load_certs(ca_pem).unwrap());
        let key = load_key(key_pem).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(tls_server(listener, certificates, key, r#"{"data": null}"#));

        // we cannot parse a configuration from text, because certificates are generally
        // added by file expansion and we don't have access to that here, and inserting
        // the PEM data directly generates parsing issues due to end of line characters
        let mut config = Configuration::default();
        config.tls.subgraph.subgraphs.insert(
            "test".to_string(),
            TlsSubgraph {
                certificate_authorities: Some(ca_pem.into()),
                client_authentication: None,
            },
        );
        let subgraph_service =
            SubgraphService::from_config("test", &config, &None, Http2Config::Enable, None)
                .unwrap();

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
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
        assert_eq!(response.response.body().data, Some(Value::Null));
    }

    async fn tls_server_with_client_auth(
        listener: tokio::net::TcpListener,
        certificates: Vec<Certificate>,
        key: PrivateKey,
        client_root: Certificate,
        body: &'static str,
    ) {
        let mut client_auth_roots = RootCertStore::empty();
        client_auth_roots.add(&client_root).unwrap();

        let client_auth = AllowAnyAuthenticatedClient::new(client_auth_roots).boxed();

        let acceptor = TlsAcceptor::builder()
            .with_tls_config(
                ServerConfig::builder()
                    .with_safe_defaults()
                    .with_client_cert_verifier(client_auth)
                    .with_single_cert(certificates, key)
                    .unwrap(),
            )
            .with_all_versions_alpn()
            .with_incoming(AddrIncoming::from_listener(listener).unwrap());
        let service = make_service_fn(|_| async {
            Ok::<_, io::Error>(service_fn(|_req| async {
                Ok::<_, io::Error>(
                    http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .version(Version::HTTP_11)
                        .body::<Body>(body.into())
                        .unwrap(),
                )
            }))
        });
        let server = Server::builder(acceptor).serve(service);
        server.await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tls_client_auth() {
        let server_certificate_pem = include_str!("./testdata/server.crt");
        let ca_pem = include_str!("./testdata/CA/ca.crt");
        let server_key_pem = include_str!("./testdata/server.key");

        let mut server_certificates = load_certs(server_certificate_pem).unwrap();
        let ca_certificate = load_certs(ca_pem).unwrap().remove(0);
        server_certificates.push(ca_certificate.clone());
        let key = load_key(server_key_pem).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(tls_server_with_client_auth(
            listener,
            server_certificates,
            key,
            ca_certificate,
            r#"{"data": null}"#,
        ));

        let client_certificate_pem = include_str!("./testdata/client.crt");
        let client_key_pem = include_str!("./testdata/client.key");

        let client_certificates = load_certs(client_certificate_pem).unwrap();
        let client_key = load_key(client_key_pem).unwrap();

        // we cannot parse a configuration from text, because certificates are generally
        // added by file expansion and we don't have access to that here, and inserting
        // the PEM data directly generates parsing issues due to end of line characters
        let mut config = Configuration::default();
        config.tls.subgraph.subgraphs.insert(
            "test".to_string(),
            TlsSubgraph {
                certificate_authorities: Some(ca_pem.into()),
                client_authentication: Some(TlsClientAuth {
                    certificate_chain: client_certificates,
                    key: client_key,
                }),
            },
        );
        let subgraph_service =
            SubgraphService::from_config("test", &config, &None, Http2Config::Enable, None)
                .unwrap();

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
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
        assert_eq!(response.response.body().data, Some(Value::Null));
    }

    // starts a local server emulating a subgraph returning status code 401
    async fn emulate_h2c_server(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            println!("h2C server got req: {_request:?}");
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response {
                        data: Some(Value::default()),
                        ..Response::default()
                    })
                    .expect("always valid")
                    .into(),
                )
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::from_tcp(listener)
            .unwrap()
            .http2_only(true)
            .serve(make_svc);
        server.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_h2c() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_h2c_server(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            Http2Config::Http2Only,
            None,
            rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
            Notify::default(),
        );

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
        assert!(response.response.body().errors.is_empty());
    }
}
