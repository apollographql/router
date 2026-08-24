//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::error::Error as _;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::task::Poll;

use futures::future::BoxFuture;
use http::StatusCode;
use http_body::Body as _;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use tower::BoxError;
use tower::Service as _;
use tower::ServiceExt;
use tracing::Instrument;

use super::http::get_uri_details;
use super::http::http_response_to_graphql_response;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::plugins::limits::response_size_limit::ResponseSizeLimitError;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::events::log_subgraph_request_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventResponse;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphRequestBodySize;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphResponseBodySize;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::http::HttpRequest;
use crate::services::http::service::WireByteCount;
use crate::services::router;
use crate::services::subgraph;

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub(crate) struct SubgraphService {
    inner: crate::services::http::BoxCloneService,
    service: Arc<String>,
}

impl SubgraphService {
    pub(crate) fn new(
        service: impl Into<String>,
        inner: crate::services::http::BoxCloneService,
    ) -> Self {
        let name = service.into();
        Self {
            inner,
            service: Arc::new(name),
        }
    }
}

impl tower::Service<SubgraphRequest> for SubgraphService {
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service_name = self.service.clone();

        let fresh_client = self.inner.clone();
        let inner = std::mem::replace(&mut self.inner, fresh_client);

        Box::pin(async move { call_http(request, inner, &service_name).await })
    }
}

/// call_http makes http calls with modified graphql::Request (body)
async fn call_http(
    request: SubgraphRequest,
    mut client: crate::services::http::BoxCloneService,
    service_name: &str,
) -> Result<SubgraphResponse, BoxError> {
    let subgraph_request_event = request
        .context
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
        context,
        id: subgraph_request_id,
        query_hash,
        ..
    } = request;

    // Used in batching to identify the query
    // XXX(@goto-bus-stop): I would prefer not to hardcode this in here, but it would require I
    // think a batching refactor to move away from query hashes entirely.
    context.extensions().with_lock(|lock| {
        lock.insert(query_hash);
    });

    let (parts, body) = subgraph_request.into_parts();
    let operation_name = body
        .operation_name
        .as_deref()
        .unwrap_or_default()
        .to_owned();
    let body = serde_json::to_string(&body)?;
    tracing::debug!("our JSON body: {body:?}");
    let request = http::Request::from_parts(parts, router::body::from_bytes(body));

    let schema_uri = request.uri();
    let (host, port, path) = get_uri_details(schema_uri);

    let subgraph_req_span = tracing::info_span!(SUBGRAPH_REQUEST_SPAN_NAME,
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %schema_uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %service_name,
        "graphql.operation.name" = %operation_name,
        "apollo.subgraph.response.aborted" = tracing::field::Empty,
    );

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

    if let Some(level) = log_request_level {
        log_subgraph_request_event(
            level,
            service_name,
            crate::services::header_masking::masked_headers_for_log(
                &context,
                crate::services::header_masking::Direction::Request,
                Some(service_name),
                request.headers(),
            ),
            request.method(),
            request.version(),
            format!("{:?}", request.body()),
            &format!("Request to subgraph {service_name:?}"),
        );
    }

    // By this point, the selectors for on_request have already run, so we store
    // the request body size in the context extensions so that any on_response selectors
    // can access it.
    if let Some(body_len) = request.size_hint().exact() {
        context.extensions().with_lock(|lock| {
            lock.insert::<SubgraphRequestBodySize>(SubgraphRequestBodySize(body_len));
        });
    }

    // Perform the actual fetch. If this fails then we didn't manage to make the call at all, so we can't do anything with it.
    let fetch_result: Result<_, FetchError> = async {
        let response = client
            .call(HttpRequest {
                http_request: request,
                context: context.clone(),
            })
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = ?err);
                FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.to_string(),
                    reason: err.to_string(),
                }
            })?;

        let (parts, response_body) = response.http_response.into_parts();
        let body = router::body::into_bytes(response_body)
            .instrument(tracing::debug_span!("aggregate_response_data"))
            .await
            .map_err(|err| {
                tracing::error!(fetch_error = ?err);
                // HACK(@goto-bus-stop): the error ends up double-boxed because we mix `axum::Error` and
                // `tower::BoxError` types, so we have to look into the source error here.
                if err
                    .source()
                    .and_then(|source| source.downcast_ref::<ResponseSizeLimitError>())
                    .is_some()
                {
                    tracing::Span::current()
                        .record("apollo.subgraph.response.aborted", "response_size_limit");
                }
                FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: service_name.to_string(),
                    reason: err.to_string(),
                }
            });

        Ok((parts, body))
    }
    .instrument(subgraph_req_span)
    .await;

    let (parts, body) = match fetch_result {
        Ok(resp) => resp,
        Err(err) => {
            return Ok(SubgraphResponse::builder()
                .subgraph_name(service_name.to_string())
                .error(err.to_graphql_error(None))
                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                .context(context)
                .extensions(Object::default())
                .build());
        }
    };

    let subgraph_response_event = context
        .extensions()
        .with_lock(|lock| lock.get::<SubgraphEventResponse>().cloned());

    if let Some(subgraph_response_event) = subgraph_response_event {
        // We have to do this in order to use selectors
        let mut resp_builder = http::Response::builder()
            .status(parts.status)
            .version(parts.version);
        if let Some(headers) = resp_builder.headers_mut() {
            *headers = parts.headers.clone();
        }
        let subgraph_response = SubgraphResponse::new_from_response(
            resp_builder
                .body(graphql::Response::default())
                .expect("it won't fail everything is coming from an existing response"),
            context.clone(),
            service_name.to_owned(),
            subgraph_request_id.clone(),
        );

        let should_log = subgraph_response_event
            .condition
            .evaluate_response(&subgraph_response);
        if should_log {
            let mut attrs = Vec::with_capacity(5);
            let headers_str = crate::services::header_masking::masked_headers_for_log(
                &context,
                crate::services::header_masking::Direction::Response,
                Some(service_name),
                &parts.headers,
            );
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.headers"),
                opentelemetry::Value::String(headers_str.into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.status"),
                opentelemetry::Value::String(format!("{}", parts.status).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.version"),
                opentelemetry::Value::String(format!("{:?}", parts.version).into()),
            ));
            if let Ok(b) = &body {
                attrs.push(KeyValue::new(
                    Key::from_static_str("http.response.body"),
                    opentelemetry::Value::String(String::from_utf8_lossy(b).to_string().into()),
                ));
            }
            attrs.push(KeyValue::new(
                Key::from_static_str("subgraph.name"),
                opentelemetry::Value::String(service_name.to_string().into()),
            ));
            log_event(
                subgraph_response_event.level,
                "subgraph.response",
                attrs,
                &format!("Raw response from subgraph {service_name:?} received"),
            );
        }
    }

    if body.is_ok()
        && let Some(wire_size) = parts
            .extensions
            .get::<WireByteCount>()
            .map(|c| c.0.load(Relaxed))
    {
        context.extensions().with_lock(|lock| {
            lock.insert::<SubgraphResponseBodySize>(SubgraphResponseBodySize(wire_size));
        });
    }

    let graphql_response = http_response_to_graphql_response(service_name, body, &parts);

    let resp = http::Response::from_parts(parts, graphql_response);
    Ok(SubgraphResponse::new_from_response(
        resp,
        context,
        service_name.to_owned(),
        subgraph_request_id,
    ))
}

/// The full, buffered service stack for one subgraph.
pub(crate) type BufferedSubgraphService =
    UnconstrainedBuffer<subgraph::Request, BoxFuture<'static, subgraph::ServiceResult>>;

/// The pre-built subgraph service stack for each subgraph, keyed by subgraph name.
/// Stacks are built once; [`Self::get`] hands out cheap clones.
#[derive(Clone)]
pub(crate) struct SubgraphServices {
    pub(crate) services: Arc<HashMap<String, BufferedSubgraphService>>,
}

impl SubgraphServices {
    /// Retrieves the pre-built subgraph service stack for `name`, or `None` if no subgraph
    /// is registered under that name.
    pub(crate) fn get(&self, name: &str) -> Option<subgraph::BoxCloneService> {
        self.services.get(name).map(|svc| svc.clone().boxed_clone())
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use SubgraphRequest;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::extract::State;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::CloseFrame;
    use axum::extract::ws::Message;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use bytes::Buf;
    use futures::StreamExt;
    use http::HeaderValue;
    use http::StatusCode;
    use http::Uri;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::header::HOST;
    use mime::APPLICATION_JSON;
    use serde_json_bytes::Value;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tower::Layer as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::Context;
    use crate::Notify;
    use crate::configuration::subgraph::SubgraphConfiguration;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::limits::response_size_limit::SubgraphResponseSizeLimit;
    use crate::plugins::subscription::CallbackMode;
    use crate::plugins::subscription::HeartbeatInterval;
    use crate::plugins::subscription::SUBSCRIPTION_CALLBACK_HMAC_KEY;
    use crate::plugins::subscription::SubgraphPassthroughMode;
    use crate::plugins::subscription::SubscriptionConfig;
    use crate::plugins::subscription::SubscriptionModeConfig;
    use crate::plugins::subscription::WebSocketConfiguration;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphService;
    use crate::protocols::websocket::ClientMessage;
    use crate::protocols::websocket::ServerError;
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::http::test_http_client_service;
    use crate::services::layers::apq::subgraph::SubgraphApqLayer;
    use crate::services::layers::apq::subgraph::SubgraphApqService;
    use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
    use crate::services::layers::content_negotiation::SubgraphContentNegotiationLayer;
    use crate::services::layers::content_negotiation::SubgraphContentNegotiationService;
    use crate::services::router;

    async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
    where
        Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
        Fut:
            std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
    {
        use hyper::body::Incoming;
        use hyper_util::rt::TokioExecutor;
        use hyper_util::rt::TokioIo;

        // Not sure this is the *right* place to do it, because it's actually clients that
        // use crypto, not the server.

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let handle = handle.clone();
            tokio::spawn(async move {
                // N.B. should use hyper service_fn here, since it's required to be implemented hyper Service trait!
                let svc = hyper::service::service_fn(|request: http::Request<Incoming>| {
                    handle(request.map(Body::new))
                });
                if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await
                {
                    eprintln!("server error: {err}");
                }
            });
        }
    }

    // starts a local server emulating a subgraph returning status code 400
    async fn emulate_subgraph_bad_request(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::BAD_REQUEST)
                .body(
                    serde_json::to_string(&Response {
                        errors: vec![
                            Error::builder()
                                .message("This went wrong")
                                .extension_code("FETCH_ERROR")
                                .build(),
                        ],
                        ..Response::default()
                    })
                    .expect("always valid")
                    .into(),
                )
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
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

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning connection closed
    #[cfg(not(target_os = "macos"))]
    async fn emulate_subgraph_panic(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            panic!("test")
        }

        let server = axum::serve(
            listener,
            Router::new().route("/", axum::routing::any_service(tower::service_fn(handle))),
        );
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

        serve(listener, handle).await.unwrap();
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

        serve(listener, handle).await.unwrap();
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

        serve(listener, handle).await.unwrap();
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

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning a large JSON response
    async fn emulate_subgraph_large_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            // 100 bytes of JSON — enough to exceed a small limit in tests
            let body = format!(r#"{{"data":{{"field":"{}"}}}}"#, "x".repeat(80));
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(body.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
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

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with missing content_type
    async fn emulate_subgraph_missing_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with invalid content_type
    async fn emulate_subgraph_invalid_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "application/json,application/json")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning unsupported content_type
    async fn emulate_subgraph_unsupported_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "text/html")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    async fn emulate_correct_websocket_server(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            let res = ws.protocols(["graphql-transport-ws"]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::text(
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
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id, payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
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
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// WebSocket server that sends one event then completes the subscription,
    /// causing the subgraph to naturally close the stream.
    async fn emulate_websocket_server_that_completes(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws.protocols(["graphql-transport-ws"]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
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
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Complete { id: client_id }).unwrap(),
                    ))
                    .await
                    .unwrap();
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    static CALLBACK_PROTOCOL_ACCEPT: HeaderValue =
        HeaderValue::from_static("application/json;callbackSpec=1.0");

    async fn emulate_subgraph_with_callback_data(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (parts, body) = request.into_parts();
            assert!(
                parts
                    .headers
                    .get_all(ACCEPT)
                    .iter()
                    .any(|header_value| header_value == CALLBACK_PROTOCOL_ACCEPT)
            );
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");
            let graphql_request = graphql_request.unwrap();
            assert!(graphql_request.extensions.contains_key("subscription"));
            let subscription_extension: crate::plugins::subscription::subgraph::SubscriptionExtension = serde_json_bytes::from_value(
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
            assert_eq!(subscription_extension.heartbeat_interval_ms, 0);

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

        serve(listener, handle).await.unwrap();
    }

    fn subscription_config() -> SubscriptionConfig {
        SubscriptionConfig {
            enabled: true,
            mode: SubscriptionModeConfig {
                callback: Some(CallbackMode {
                    public_url: Url::parse("http://localhost:4000/testcallback").unwrap(),
                    listen: None,
                    path: Some("/testcallback".to_string()),
                    subgraphs: vec![String::from("testbis")].into_iter().collect(),
                    heartbeat_interval: HeartbeatInterval::new_disabled(),
                }),
                passthrough: Some(SubgraphPassthroughMode {
                    all: None,
                    subgraphs: [(
                        "test".to_string(),
                        WebSocketConfiguration {
                            path: Some(String::from("/ws")),
                            protocol: WebSocketProtocol::default(),
                            heartbeat_interval: HeartbeatInterval::new_disabled(),
                            max_reconnect_attempts: 0,
                            reconnect_delay: None,
                        },
                    )]
                    .into(),
                }),
            },
            deduplication: SubgraphConfiguration::default(),
            max_opened_subscriptions: None,
            queue_capacity: None,
            max_lifetime: None,
        }
    }

    /// Wraps a bare `SubgraphService` with `SubgraphLayer`, mirroring the position it occupies in
    /// `SubgraphServices::new`. Most unit tests construct a `SubgraphService` directly and
    /// call it without going through `SubgraphServices`, which would otherwise skip
    /// Accept/Content-Type header injection entirely.
    fn with_content_negotiation_layer(
        s: SubgraphService,
    ) -> SubgraphContentNegotiationService<SubgraphService> {
        SubgraphContentNegotiationLayer::default().layer(s)
    }

    /// Manually rebuilds the production layer stack (Subscription -> APQ -> SubgraphLayer ->
    /// SubgraphService) for subscriptions tests, which construct a `SubgraphService` directly
    /// instead of going through the `SubgraphServices`.
    fn with_subscription_layer(
        s: SubgraphService,
    ) -> SubscriptionSubgraphService<
        SubgraphApqService<SubgraphContentNegotiationService<SubgraphService>>,
    > {
        ServiceBuilder::new()
            .layer(SubscriptionSubgraphLayer::new(
                Notify::builder().build(),
                Some(Arc::new(subscription_config())),
                Arc::from(s.service.to_string()),
            ))
            .layer(SubgraphApqLayer::new(false))
            .layer(SubgraphContentNegotiationLayer::default())
            .service(s)
    }

    fn supergraph_request(query: &str) -> Arc<http::Request<Request>> {
        Arc::new(
            http::Request::builder()
                .header(HOST, "host")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(Request::builder().query(query).build())
                .expect("expecting valid request"),
        )
    }

    fn subgraph_http_request(uri: Uri, query: &str) -> http::Request<Request> {
        http::Request::builder()
            .header(HOST, "rhost")
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .uri(uri)
            .body(Request::builder().query(query).build())
            .expect("expecting valid request")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_callback() {
        let _ = SUBSCRIPTION_CALLBACK_HMAC_KEY.set(String::from("TESTEST"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_subgraph_with_callback_data(listener));
        let subgraph_service = with_subscription_layer(SubgraphService::new(
            "testbis",
            test_http_client_service("testbis"),
        ));
        let (tx, _rx) = mpsc::channel(2);
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_graphql_response(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_content_type_application_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_json_response(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(target_os = "macos"))]
    async fn test_subgraph_service_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_panic(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(!response.response.body().errors.is_empty());
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: HTTP fetch failed: connection closed before message completed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_invalid_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_ok_status_invalid_response(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_response_size_limit_exceeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_large_response(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let context = Context::new();
        context
            .extensions()
            .with_lock(|e| e.insert(SubgraphResponseSizeLimit(10)));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(context)
                    .build(),
            )
            .await
            .unwrap();

        let errors = &response.response.body().errors;
        assert!(!errors.is_empty(), "expected an error for exceeded limit");
        assert!(
            errors[0].message.contains("exceeded limit of 10 bytes"),
            "unexpected error message: {}",
            errors[0].message
        );
        assert_eq!(
            errors[0].extensions.get("code").and_then(|v| v.as_str()),
            Some("SUBREQUEST_HTTP_ERROR")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_response_size_limit_under() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_json_response(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let context = Context::new();
        // Limit of 1000 bytes — well above {"data": null} (14 bytes)
        context
            .extensions()
            .with_lock(|e| e.insert(SubgraphResponseSizeLimit(1000)));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(context)
                    .build(),
            )
            .await
            .unwrap();

        assert!(
            response.response.body().errors.is_empty(),
            "expected no errors when response is under the limit"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_json(listener),
        );
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "response was malformed: invalid"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_graphql() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_graphql(listener),
        );
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_correct_websocket_server(listener));
        let subgraph_service = with_subscription_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));
        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);

        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();
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
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            tokio::task::spawn(emulate_incorrect_websocket_server(listener));
            let subgraph_service = with_subscription_layer(
                SubgraphService::new("test", test_http_client_service("test")),
            );
            let (tx, _rx) = mpsc::channel(2);

            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
            let err = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap_err();

            let err_str = err.to_string();
            assert!(err_str.starts_with("Websocket fetch failed: cannot connect websocket to subgraph: WebSocket upgrade failed. Status: 400 Bad Request; Headers: [\"content-type\": \"text/plain; charset=utf-8\"; \"content-length\": \"11\";"));

            assert_counter!(
                "apollo.router.operations.subscriptions.rejected",
                1,
                "reason" = "subgraph",
                "subgraph.name" = "test"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket_ended_increments_counter() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let spawned_task =
                tokio::task::spawn(emulate_websocket_server_that_completes(listener));
            let subgraph_service = with_subscription_layer(SubgraphService::new(
                "test",
                test_http_client_service("test"),
            ));
            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);

            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();
            let message = gql_stream.next().await.unwrap();
            assert_eq!(
                message,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Drain remaining messages until the stream ends (server sent Complete)
            while gql_stream.next().await.is_some() {}

            // The ended counter is incremented in a spawned task after gql_stream
            // completes forwarding; yield briefly to let it run.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// WebSocket server that sends one event per connection and then holds the connection open
    /// indefinitely (never closes it, never errors) — so a forwarding task reading from it can
    /// only end via its client-departure path (the closing signal), never via the subgraph
    /// itself ending the stream.
    async fn emulate_websocket_server_that_stays_open(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws.protocols(["graphql-transport-ws"]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
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
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id, payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                // Hold the connection open. The router closes it (by dropping `gql_stream`)
                // only once the forwarding task's whole async block ends.
                while let Some(Ok(_)) = socket.recv().await {}
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Regression test for a deduplication race: when the last client for a deduplicated
    /// subscription leaves right as a textually-identical subscription from a new client
    /// arrives, the new client's subscription must survive the old one's teardown rather than
    /// being silently killed by a stale `ForceDelete` targeting the (by-then re-created) topic.
    ///
    /// Uses the default (`current_thread`) test runtime rather than `flavor = "multi_thread"` so
    /// the interleaving below is deterministic: the client-1 unsubscribe and the client-2
    /// resubscribe are queued back to back on the test's own task without yielding, so the
    /// pubsub actor — a separate task, only scheduled once this task actually suspends — always
    /// processes them in that order (deleting, then re-creating, the topic) before the old
    /// forwarding task, only just woken by the deletion's closing signal, gets a chance to run
    /// its own teardown.
    #[tokio::test]
    async fn test_dedup_client_departure_does_not_kill_recreated_subscription() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let spawned_task =
                tokio::task::spawn(emulate_websocket_server_that_stays_open(listener));

            // A single shared `Notify` (unlike `with_subscription_layer`, which would hand each
            // call its own) so both requests below hit the same deduplication topic.
            let subgraph_service = SubscriptionSubgraphLayer::new(
                Notify::builder().build(),
                Some(Arc::new(subscription_config())),
                Arc::from("test"),
            )
            .layer(SubgraphService::new(
                "test",
                test_http_client_service("test"),
            ));

            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
            let query = "subscription {\n  userWasCreated {\n    username\n  }\n}";

            // Client 1 creates the deduplicated topic and connects to the subgraph.
            let (tx1, rx1) = mpsc::channel(2);
            let mut rx_stream1 = ReceiverStream::new(rx1);
            let response1 = subgraph_service
                .clone()
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(query))
                        .subgraph_request(subgraph_http_request(url.clone(), query))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx1)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response1.response.body().errors.is_empty());
            let mut gql_stream1 = rx_stream1.next().await.unwrap();
            assert!(
                gql_stream1.next().await.is_some(),
                "client 1 should receive the first event"
            );

            // Client 1 — the only subscriber — disconnects. This immediately (synchronously,
            // via `HandleGuard::drop`) queues a receiver-count-guarded `Unsubscribe` for the
            // pubsub actor; the old forwarding task hasn't reacted to it yet.
            drop(gql_stream1);

            // A textually-identical request from a new client arrives right away, with no
            // intervening yield — queuing its `CreateOrSubscribe` immediately behind client 1's
            // `Unsubscribe` on the same task, before the pubsub actor or the old forwarding task
            // have run at all.
            let (tx2, rx2) = mpsc::channel(2);
            let mut rx_stream2 = ReceiverStream::new(rx2);
            let response2 = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(query))
                        .subgraph_request(subgraph_http_request(url, query))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx2)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response2.response.body().errors.is_empty());
            let mut gql_stream2 = rx_stream2.next().await.unwrap();

            // Give the old forwarding task's belated teardown every opportunity to run — and,
            // pre-fix, to send its stale `ForceDelete` — before checking that client 2 survived.
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }

            let message = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                gql_stream2.next(),
            )
            .await
            .expect("client 2's subscription must not be silently killed by client 1's teardown")
            .expect("client 2's stream ended instead of delivering data");
            assert_eq!(
                message,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// Verifies that a server-sent Complete message does NOT trigger reconnection, even when
    /// max_reconnect_attempts > 0. A Complete ends the stream with a terminal `None`, which is
    /// never treated as a recoverable drop.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_complete_does_not_reconnect() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let spawned_task =
                tokio::task::spawn(emulate_websocket_server_that_completes(listener));

            // Configure reconnect — the Complete should suppress it entirely.
            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                5,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // One event from the server before Complete.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Stream ends cleanly — no reconnect attempt, no second event. The forwarding task
            // increments its metrics strictly before closing `handle_sink` (the event that
            // produces this `None`), so no extra wait is needed here.
            assert!(gql_stream.next().await.is_none());

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // Exactly one completion event for the logical subscription, even across the (here,
            // single) physical connection.
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                1,
                subscriptions.mode = "passthrough",
                subscriptions.complete = true
            );
            // Reconnect counter must remain zero.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// WebSocket server that tracks connection count via shared atomic.
    /// - First connection: sends one event then drops with an abnormal close (triggers reconnect).
    /// - Subsequent connections: sends one event and stays open (simulates successful reconnect).
    async fn emulate_websocket_server_with_reconnect(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        ws.protocols(["graphql-transport-ws"])
                            .on_upgrade(move |mut socket| async move {
                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                assert!(matches!(
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                    ClientMessage::ConnectionInit { .. }
                                ));
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::ConnectionAck)
                                            .unwrap(),
                                    ))
                                    .await
                                    .unwrap();

                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                let client_id = if let ClientMessage::Subscribe { id, .. } =
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                {
                                    id
                                } else {
                                    panic!("expected Subscribe message");
                                };

                                let username =
                                    if conn_num == 0 { "ada_lovelace" } else { "grace_hopper" };
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::Next {
                                            id: client_id.clone(),
                                            payload: graphql::Response::builder()
                                                .data(serde_json_bytes::json!({"userWasCreated": {"username": username}}))
                                                .build(),
                                        })
                                        .unwrap(),
                                    ))
                                    .await
                                    .unwrap();

                                if conn_num == 0 {
                                    // Simulate unexpected connection drop with an abnormal close
                                    // frame (code 1011), which surfaces as a `Disconnected` event.
                                    // A Normal close or Complete would end the stream with a
                                    // terminal `None` and suppress reconnection.
                                    socket
                                        .send(Message::Close(Some(CloseFrame {
                                            code: 1011,
                                            reason: "unexpected termination".into(),
                                        })))
                                        .await
                                        .unwrap();
                                }
                                // Subsequent connections: hold open until the test aborts the task.
                            })
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Same as [`emulate_websocket_server_with_reconnect`], but speaks the legacy
    /// subscriptions-transport-ws protocol: negotiates the "graphql-ws" subprotocol, expects an
    /// `OldStart` message rather than `Subscribe`, and sends subscription events as a raw
    /// `type: "data"` message (rather than `type: "next"`) to exercise the client's
    /// `#[serde(alias = "data")]` handling.
    async fn emulate_websocket_server_with_reconnect_legacy_protocol(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        ws.protocols(["graphql-ws"])
                            .on_upgrade(move |mut socket| async move {
                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                assert!(matches!(
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                    ClientMessage::ConnectionInit { .. }
                                ));
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::ConnectionAck)
                                            .unwrap(),
                                    ))
                                    .await
                                    .unwrap();

                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                let client_id = if let ClientMessage::OldStart { id, .. } =
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                {
                                    id
                                } else {
                                    panic!("expected OldStart message");
                                };

                                let username =
                                    if conn_num == 0 { "ada_lovelace" } else { "grace_hopper" };
                                socket
                                    .send(Message::text(format!(
                                        r#"{{"type":"data","id":"{client_id}","payload":{{"data":{{"userWasCreated":{{"username":"{username}"}}}}}}}}"#
                                    )))
                                    .await
                                    .unwrap();

                                if conn_num == 0 {
                                    // Simulate unexpected connection drop with an abnormal close
                                    // frame (code 1011), which surfaces as a `Disconnected` event.
                                    socket
                                        .send(Message::Close(Some(CloseFrame {
                                            code: 1011,
                                            reason: "unexpected termination".into(),
                                        })))
                                        .await
                                        .unwrap();
                                }
                                // Subsequent connections: hold open until the test aborts the task.
                            })
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// First connection: completes the handshake, sends one event, then drops with an abnormal
    /// close (triggering a reconnect). Every subsequent connection refuses the WebSocket upgrade
    /// (HTTP 500), so the reconnect handshake fails inside `open_ws_gql_stream`. Used to verify a
    /// failed *reconnect* handshake does not increment the `rejected` counter.
    async fn emulate_websocket_server_rejects_reconnect(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        if conn_num > 0 {
                            // Refuse the upgrade so the reconnect handshake fails.
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no upgrade")
                                .into_response();
                        }
                        ws.protocols(["graphql-transport-ws"])
                            .on_upgrade(move |mut socket| async move {
                                let msg =
                                    socket.recv().await.unwrap().unwrap().into_text().unwrap();
                                assert!(matches!(
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                    ClientMessage::ConnectionInit { .. }
                                ));
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::ConnectionAck)
                                            .unwrap(),
                                    ))
                                    .await
                                    .unwrap();
                                let msg =
                                    socket.recv().await.unwrap().unwrap().into_text().unwrap();
                                let client_id = if let ClientMessage::Subscribe { id, .. } =
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                {
                                    id
                                } else {
                                    panic!("expected Subscribe message");
                                };
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::Next {
                                            id: client_id,
                                            payload: graphql::Response::builder()
                                                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                                .build(),
                                        })
                                        .unwrap(),
                                    ))
                                    .await
                                    .unwrap();
                                socket
                                    .send(Message::Close(Some(CloseFrame {
                                        code: 1011,
                                        reason: "unexpected termination".into(),
                                    })))
                                    .await
                                    .unwrap();
                            })
                            .into_response()
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Like `emulate_websocket_server_that_completes` but simulates an unexpected connection drop
    /// (abnormal close frame) instead of a protocol-level Complete. Used to test reconnect logic:
    /// the drop surfaces as a `Disconnected` event rather than a terminal `None`.
    async fn emulate_websocket_server_that_drops(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws
                .protocols(["graphql-transport-ws"])
                .on_upgrade(move |mut socket| async move {
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    assert!(matches!(
                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                        ClientMessage::ConnectionInit { .. }
                    ));
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let client_id =
                        if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::Next {
                                id: client_id,
                                payload: graphql::Response::builder()
                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                    .build(),
                            })
                            .unwrap(),
                        ))
                        .await
                        .unwrap();
                    // Abnormal close — surfaces as a `Disconnected` event, triggering reconnect.
                    socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 1011,
                            reason: "unexpected termination".into(),
                        })))
                        .await
                        .unwrap();
                });
            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// First connection: completes the full graphql-ws handshake, sends one event, then
    /// drops with an abnormal close frame (triggering reconnect logic). Every subsequent
    /// connection stalls — the axum handler returns `pending` forever so the TCP connection
    /// is established but the HTTP upgrade response never arrives. Used to verify that the
    /// `subscription_closing_signal` select! arms abort the reconnect before the handshake
    /// completes (or even before the delay expires).
    async fn emulate_websocket_server_drops_then_stalls(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        if conn_num > 0 {
                            // Stall: never send an HTTP response, so connect_async hangs.
                            std::future::pending::<axum::response::Response>().await
                        } else {
                            ws.protocols(["graphql-transport-ws"])
                                .on_upgrade(move |mut socket| async move {
                                    let msg = socket
                                        .recv()
                                        .await
                                        .unwrap()
                                        .unwrap()
                                        .into_text()
                                        .unwrap();
                                    assert!(matches!(
                                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                        ClientMessage::ConnectionInit { .. }
                                    ));
                                    socket
                                        .send(Message::text(
                                            serde_json::to_string(&ServerMessage::ConnectionAck)
                                                .unwrap(),
                                        ))
                                        .await
                                        .unwrap();
                                    let msg = socket
                                        .recv()
                                        .await
                                        .unwrap()
                                        .unwrap()
                                        .into_text()
                                        .unwrap();
                                    let client_id =
                                        if let ClientMessage::Subscribe { id, .. } =
                                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                        {
                                            id
                                        } else {
                                            panic!("expected Subscribe message");
                                        };
                                    socket
                                        .send(Message::text(
                                            serde_json::to_string(&ServerMessage::Next {
                                                id: client_id,
                                                payload: graphql::Response::builder()
                                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                                    .build(),
                                            })
                                            .unwrap(),
                                        ))
                                        .await
                                        .unwrap();
                                    socket
                                        .send(Message::Close(Some(CloseFrame {
                                            code: 1011,
                                            reason: "unexpected termination".into(),
                                        })))
                                        .await
                                        .unwrap();
                                })
                                .into_response()
                        }
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Like `emulate_websocket_server_that_drops` but holds each connection open for
    /// `hold` after sending its single event, then drops with an abnormal close. The
    /// connection count is reported back so the test can stop the server once it has
    /// observed enough reconnect cycles.
    async fn emulate_websocket_server_stable_then_drops(
        listener: TcpListener,
        hold: std::time::Duration,
        max_drops: u32,
        connection_count: Arc<AtomicU32>,
    ) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
            State((hold, max_drops, connection_count)): State<(
                std::time::Duration,
                u32,
                Arc<AtomicU32>,
            )>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws
                .protocols(["graphql-transport-ws"])
                .on_upgrade(move |mut socket| async move {
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    assert!(matches!(
                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                        ClientMessage::ConnectionInit { .. }
                    ));
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let client_id =
                        if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::Next {
                                id: client_id,
                                payload: graphql::Response::builder()
                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                    .build(),
                            })
                            .unwrap(),
                        ))
                        .await
                        .unwrap();
                    // Keep the connection open long enough for the router to treat it
                    // as stable (past the grace window).
                    tokio::time::sleep(hold).await;
                    // Drop only the first `max_drops` connections; hold any later connection open
                    // so the reconnect count is bounded and the test is deterministic (no extra
                    // reconnect can race the assertion).
                    let drop_index = connection_count.fetch_add(1, Ordering::SeqCst);
                    if drop_index < max_drops {
                        socket
                            .send(Message::Close(Some(CloseFrame {
                                code: 1011,
                                reason: "unexpected termination".into(),
                            })))
                            .await
                            .unwrap();
                    } else {
                        // Hold open until the test aborts the task.
                        std::future::pending::<()>().await;
                    }
                });
            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler)).with_state((
            hold,
            max_drops,
            connection_count,
        ));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Completes the handshake, sends a terminal operation `Error` (application-level, not a
    /// transport error code), then drops the connection with an abnormal close. The terminal
    /// Error must prevent the router from reconnecting even though the close is abnormal.
    async fn emulate_websocket_server_sends_error_then_drops(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
            State(count): State<Arc<AtomicU32>>,
        ) -> Result<impl IntoResponse, Infallible> {
            count.fetch_add(1, Ordering::SeqCst);
            let res =
                ws.protocols(["graphql-transport-ws"])
                    .on_upgrade(move |mut socket| async move {
                        let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                        assert!(matches!(
                            serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                            ClientMessage::ConnectionInit { .. }
                        ));
                        socket
                            .send(Message::text(
                                serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                            ))
                            .await
                            .unwrap();
                        let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                        let client_id = if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                        // Terminal operation error from the subgraph (not a transport error code).
                        socket
                            .send(Message::text(
                                serde_json::to_string(&ServerMessage::Error {
                                    id: Some(client_id),
                                    payload: ServerError::Error(
                                        Error::builder()
                                            .message("boom")
                                            .extension_code("MY_SUBGRAPH_ERROR")
                                            .build(),
                                    ),
                                })
                                .unwrap(),
                            ))
                            .await
                            .unwrap();
                        // Abnormal close after the terminal error — must NOT trigger a reconnect.
                        socket
                            .send(Message::Close(Some(CloseFrame {
                                code: 1011,
                                reason: "unexpected termination".into(),
                            })))
                            .await
                            .unwrap();
                    });
            Ok(res)
        }

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(connection_count);
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    fn subscription_config_with_reconnect(max_reconnect_attempts: u32) -> SubscriptionConfig {
        subscription_config_with_reconnect_delay(
            max_reconnect_attempts,
            std::time::Duration::from_millis(1),
        )
    }

    fn subscription_config_with_reconnect_delay(
        max_reconnect_attempts: u32,
        reconnect_delay: std::time::Duration,
    ) -> SubscriptionConfig {
        // Reconnect policy now lives on the per-subgraph WebSocketConfiguration; set it on the
        // "test" subgraph's passthrough config.
        let mut config = subscription_config();
        if let Some(passthrough) = &mut config.mode.passthrough
            && let Some(ws) = passthrough.subgraphs.get_mut("test")
        {
            ws.max_reconnect_attempts = max_reconnect_attempts;
            ws.reconnect_delay = Some(reconnect_delay);
        }
        config
    }

    fn with_subscription_layer_reconnect(
        s: SubgraphService,
        max_reconnect_attempts: u32,
    ) -> SubscriptionSubgraphService<SubgraphService> {
        SubscriptionSubgraphLayer::new(
            crate::plugins::subscription::notification::Notify::builder().build(),
            Some(Arc::new(subscription_config_with_reconnect(
                max_reconnect_attempts,
            ))),
            Arc::from(s.service.to_string()),
        )
        .layer(s)
    }

    fn subscription_config_with_reconnect_protocol(
        max_reconnect_attempts: u32,
        protocol: WebSocketProtocol,
    ) -> SubscriptionConfig {
        let mut config = subscription_config_with_reconnect(max_reconnect_attempts);
        if let Some(passthrough) = &mut config.mode.passthrough
            && let Some(ws) = passthrough.subgraphs.get_mut("test")
        {
            ws.protocol = protocol;
        }
        config
    }

    fn with_subscription_layer_reconnect_protocol(
        s: SubgraphService,
        max_reconnect_attempts: u32,
        protocol: WebSocketProtocol,
    ) -> SubscriptionSubgraphService<SubgraphService> {
        SubscriptionSubgraphLayer::new(
            crate::plugins::subscription::notification::Notify::builder().build(),
            Some(Arc::new(subscription_config_with_reconnect_protocol(
                max_reconnect_attempts,
                protocol,
            ))),
            Arc::from(s.service.to_string()),
        )
        .layer(s)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicU32::new(0));
        let spawned_task = tokio::task::spawn(emulate_websocket_server_with_reconnect(
            listener,
            connection_count.clone(),
        ));

        let subgraph_service = with_subscription_layer_reconnect(
            SubgraphService::new("test", test_http_client_service("test")),
            1,
        );

        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);
        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();

        // First event comes from the initial connection.
        let first = gql_stream.next().await.unwrap();
        assert_eq!(
            first,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );

        // Transient transport errors from the abnormal close are suppressed during the
        // reconnect window (so HTTP-multipart clients don't tear down). The next item the
        // client sees is data from the reconnected stream. Loop defensively in case any
        // unexpected error item slips through.
        let second = loop {
            let item = gql_stream.next().await.unwrap();
            if item.errors.is_empty() {
                break item;
            }
        };
        assert_eq!(
            second,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "grace_hopper"}}))
                .build()
        );

        spawned_task.abort();
    }

    /// Same scenario as `test_websocket_reconnect_succeeds`, but against a subgraph speaking the
    /// legacy subscriptions-transport-ws protocol (`OldStart`/`OldStop`, "graphql-ws"
    /// subprotocol, `type: "data"` events) rather than graphql-ws (`Subscribe`/`Complete`,
    /// "graphql-transport-ws" subprotocol, `type: "next"`). Confirms reconnect works
    /// independently of which WebSocket protocol the subgraph uses.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_succeeds_legacy_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicU32::new(0));
        let spawned_task =
            tokio::task::spawn(emulate_websocket_server_with_reconnect_legacy_protocol(
                listener,
                connection_count.clone(),
            ));

        let subgraph_service = with_subscription_layer_reconnect_protocol(
            SubgraphService::new("test", test_http_client_service("test")),
            1,
            WebSocketProtocol::SubscriptionsTransportWs,
        );

        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);
        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();

        // First event comes from the initial connection.
        let first = gql_stream.next().await.unwrap();
        assert_eq!(
            first,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );

        // Transient transport errors from the abnormal close are suppressed during the
        // reconnect window. The next item the client sees is data from the reconnected stream.
        let second = loop {
            let item = gql_stream.next().await.unwrap();
            if item.errors.is_empty() {
                break item;
            }
        };
        assert_eq!(
            second,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "grace_hopper"}}))
                .build()
        );

        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_exhausted_increments_counter() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            // emulate_websocket_server_that_drops sends one event then an abnormal close frame
            // on every connection, so every attempt (initial + reconnects) triggers reconnect logic.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let gql_stream = rx_stream.next().await;
            assert!(
                gql_stream.is_some(),
                "expected subscription stream from channel"
            );
            let mut gql_stream = gql_stream.unwrap();

            // Event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");
            assert_eq!(
                first.unwrap(),
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Errors from the first abnormal close are suppressed during the reconnect
            // window; the next item should be data from the reconnected stream.
            let second = loop {
                let item = gql_stream.next().await;
                assert!(item.is_some(), "stream ended before second data event");
                let item = item.unwrap();
                if item.errors.is_empty() {
                    break item;
                }
            };
            assert_eq!(
                second,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Drain remaining items (errors from second drop) until the stream terminates. The
            // forwarding task increments its metrics strictly before closing `handle_sink` (the
            // event that ends this stream), so no extra wait is needed once it's drained.
            while gql_stream.next().await.is_some() {}

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                1,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// Verifies the default behavior: when `max_reconnect_attempts` is 0 (equivalent to the
    /// unset default via `unwrap_or(0)` in subgraph.rs), an abnormal subgraph disconnect must
    /// terminate the subscription immediately — no reconnect attempt, no reconnect counter.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_drop_does_not_reconnect_when_attempts_zero() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            // Sends one event and then an abnormal close on every connection. With
            // max_reconnect_attempts=0 we expect only the first event to reach the client.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                0,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Event from the initial (and only) connection.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // After the abnormal close the stream must terminate without producing another data
            // item. Windows may surface one or more error items from the close frame before the
            // stream ends; drain those and assert no data follows.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => {
                        panic!("unexpected data after subgraph drop with max_reconnect_attempts=0")
                    }
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // The drop is the terminal end of the subscription on the subgraph side.
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // No reconnect was attempted.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// A connection that stays open past the grace window before dropping should
    /// refresh the per-disconnect retry budget. With `max_reconnect_attempts=1`,
    /// a server that drops after every "stable" connection should produce *more
    /// than one* reconnect — the budget resets on each drop. A hard lifetime
    /// ceiling would terminate after exactly one reconnect.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_budget_resets_after_stable_connection() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // The grace floor is 500ms (see `stability_grace` in subgraph.rs); hold each
            // connection 600ms so every drop is past it. Drop only the first 2 connections, then
            // hold the third open — so exactly 2 reconnects happen and no later reconnect can race
            // the assertion below.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_stable_then_drops(
                listener,
                std::time::Duration::from_millis(600),
                2,
                connection_count.clone(),
            ));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Pull data events through several reconnect cycles. After observing
            // 3 successful "stable" connections (one initial + 2 reconnects), we
            // know the budget was refreshed at least once — a hard ceiling would
            // have terminated after the first reconnect with max_reconnect_attempts=1.
            let mut data_events = 0u32;
            while data_events < 3 {
                let item = gql_stream.next().await.unwrap();
                if !item.errors.is_empty() {
                    continue;
                }
                assert_eq!(item.subscribed, Some(true));
                data_events += 1;
            }

            // The reconnect counter is incremented strictly before the reconnected stream's data
            // is forwarded to this client, so observing the 3rd data event above already
            // guarantees both reconnects are reflected in the metrics — no extra wait is needed.

            // Exactly 2 reconnects happened to reach 3 data events (the third connection is held
            // open, so no further reconnect occurs). With a hard ceiling on attempts
            // (`max_reconnect_attempts=1`), the counter would be capped at 1 and the subscription
            // would have terminated before the third event.
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                2,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// A failed *reconnect* handshake must increment the reconnect counter but NOT the
    /// `rejected` counter, which tracks rejected subscription requests rather than reconnect
    /// failures. The initial connect succeeds (so `rejected` is never touched there); the single
    /// reconnect attempt fails the WebSocket upgrade.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_failed_reconnect_does_not_increment_rejected() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            let spawned_task = tokio::task::spawn(emulate_websocket_server_rejects_reconnect(
                listener,
                connection_count.clone(),
            ));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Event from the initial (successful) connection.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // The reconnect handshake fails; after the single attempt is exhausted the stream
            // terminates. Drain any error items, then assert the stream ends.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => panic!("unexpected data after failed reconnect"),
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // One reconnect attempt was issued (and failed).
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                1,
                "subgraph.name" = "test"
            );
            // The subscription ultimately terminated subgraph-side.
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // A failed reconnect handshake must NOT be counted as a rejected subscription request.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.rejected",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When reconnection is exhausted, the last suppressed transport error must be forwarded to
    /// the client so a failed subscription is distinguishable from a normal completion. The server
    /// drops every connection with an abnormal close, so after `max_reconnect_attempts` the
    /// subscription ends with a terminal transport error rather than a silent stream end.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_exhausted_forwards_terminal_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

        let subgraph_service = with_subscription_layer_reconnect(
            SubgraphService::new("test", test_http_client_service("test")),
            1,
        );

        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);
        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();

        // Consume the whole stream. Transport errors are suppressed during the reconnect window;
        // only the terminal one (after attempts are exhausted) should reach the client.
        let mut data_events = 0u32;
        let mut terminal_error = None;
        while let Some(item) = gql_stream.next().await {
            if item.errors.is_empty() {
                data_events += 1;
            } else {
                terminal_error = Some(item);
            }
        }

        assert!(data_events >= 1, "expected at least the initial data event");
        let terminal_error = terminal_error
            .expect("client should receive a terminal error after reconnect exhausted");
        assert_eq!(terminal_error.subscribed, Some(false));
        // The terminal error is whatever transport error ended the last connection. An abnormal
        // close surfaces as WEBSOCKET_CLOSE_ERROR on most platforms, but can arrive as a read
        // failure (WEBSOCKET_MESSAGE_ERROR) depending on socket timing (e.g. on Windows).
        assert!(
            terminal_error.errors.iter().any(|e| matches!(
                e.extension_code().as_deref(),
                Some("WEBSOCKET_CLOSE_ERROR") | Some("WEBSOCKET_MESSAGE_ERROR")
            )),
            "terminal error should carry a transport error code, got: {:?}",
            terminal_error.errors
        );

        spawned_task.abort();
    }

    /// A terminal operation `Error` from the subgraph ends the subscription server-side. Even
    /// though the subgraph then drops the connection abnormally, the router must NOT reconnect
    /// (the Error marks the stream server-ended, so the following close is the expected teardown),
    /// and the client must receive the application error.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_application_error_does_not_reconnect() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            let spawned_task = tokio::task::spawn(emulate_websocket_server_sends_error_then_drops(
                listener,
                connection_count.clone(),
            ));

            // Reconnect is configured — the terminal Error must suppress it entirely.
            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new("test", test_http_client_service("test")),
                5,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // The client must receive the application error.
            let app_error = gql_stream.next().await.unwrap();
            assert!(
                app_error
                    .errors
                    .iter()
                    .any(|e| e.extension_code().as_deref() == Some("MY_SUBGRAPH_ERROR")),
                "client should receive the subgraph application error"
            );

            // After the terminal error the stream ends; no data from a reconnected stream.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => panic!("unexpected data after a terminal application error"),
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // Exactly one connection was made — no reconnect was attempted.
            assert_eq!(connection_count.load(Ordering::SeqCst), 1);
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When all clients drop while the router is sleeping during the reconnect delay, the
    /// router must abort without attempting the reconnect handshake at all. This exercises
    /// the `biased select!` arm on `subscription_closing_signal` inside the delay sleep.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_closing_signal_during_delay() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // Server stalls on the second connection; if the router incorrectly attempts
            // a reconnect the stall will hold the test open and the counter assertion below
            // will fail.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_drops_then_stalls(
                listener,
                connection_count.clone(),
            ));

            // Use a long reconnect delay (200 ms) so the test can reliably drop the client
            // stream before the delay expires and the reconnect handshake starts.
            let reconnect_delay = std::time::Duration::from_millis(200);
            let subgraph_service = SubscriptionSubgraphLayer::new(
                crate::plugins::subscription::notification::Notify::builder().build(),
                Some(Arc::new(subscription_config_with_reconnect_delay(
                    3,
                    reconnect_delay,
                ))),
                Arc::from("test"),
            )
            .layer(
                SubgraphService::new("test", test_http_client_service("test")),
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Receive the first (and only) event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");

            // Drop the stream — simulates all clients disconnecting.
            // The router is now sleeping in the 200 ms reconnect delay; the closing signal
            // should interrupt it before the delay expires.
            drop(gql_stream);

            // Wait for the forwarding task to reach its terminal teardown — the point,
            // immediately before `handle_sink.close()`, where it emits this completion metric —
            // instead of sleeping a fixed duration. A broken implementation that ignored the
            // closing signal would sleep out the full delay and then dial the stalling server,
            // hanging on its handshake forever; this metric would then never appear, so the wait
            // below times out and fails the test instead of passing silently.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !crate::metrics::collect_metrics().metric_exists(
                    "apollo.router.operations.subscriptions.events",
                    crate::metrics::test_utils::MetricType::Counter,
                    &[
                        opentelemetry::KeyValue::new("subscriptions.mode", "passthrough"),
                        opentelemetry::KeyValue::new("subscriptions.complete", true),
                    ],
                ) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect(
                "expected the closing signal to abort the reconnect delay and the forwarding task to complete",
            );

            // The closing signal must have fired during the delay sleep — no reconnect
            // handshake should have been issued.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );
            // The server should never have seen a second connection.
            assert_eq!(
                connection_count.load(Ordering::SeqCst),
                1,
                "router must not attempt a reconnect after all clients disconnect"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When all clients drop while the reconnect handshake is in progress (TCP connected,
    /// waiting for the HTTP upgrade response), the router must abort without completing
    /// the handshake and must NOT increment the reconnect counter. This exercises the
    /// `biased select!` arm on `subscription_closing_signal` inside `open_ws_gql_stream`.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_closing_signal_during_handshake() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // Server stalls on the second connection's HTTP upgrade so that the reconnect
            // handshake hangs long enough for the test to drop the client stream.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_drops_then_stalls(
                listener,
                connection_count.clone(),
            ));

            // Use a very short reconnect delay so the handshake starts almost immediately
            // after the connection drops; the test then drops the client stream while the
            // handshake is in progress.
            let subgraph_service = SubscriptionSubgraphLayer::new(
                crate::plugins::subscription::notification::Notify::builder().build(),
                Some(Arc::new(subscription_config_with_reconnect_delay(
                    3,
                    std::time::Duration::from_millis(1),
                ))),
                Arc::from("test"),
            )
            .layer(
                SubgraphService::new("test", test_http_client_service("test")),
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Receive the first event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");

            // Poll until the reconnect handshake has actually dialed the stalling server before
            // dropping the stream — otherwise the assertions below would pass trivially because
            // the handshake was never attempted. This avoids racing a fixed sleep against the
            // 1 ms reconnect delay and the TCP connect.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while connection_count.load(Ordering::SeqCst) < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect(
                "expected the reconnect handshake to have dialed the stalling server within 30s",
            );
            drop(gql_stream);

            // Wait for the forwarding task to reach its terminal teardown — the point,
            // immediately before `handle_sink.close()`, where it emits this completion metric —
            // instead of a fixed sleep. Because `emulate_websocket_server_drops_then_stalls`
            // never completes the handshake, a broken closing-signal abort would leave the task
            // hung on it forever and this metric would never appear, so the wait below times out
            // and fails the test instead of passing silently.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !crate::metrics::collect_metrics().metric_exists(
                    "apollo.router.operations.subscriptions.events",
                    crate::metrics::test_utils::MetricType::Counter,
                    &[
                        opentelemetry::KeyValue::new("subscriptions.mode", "passthrough"),
                        opentelemetry::KeyValue::new("subscriptions.complete", true),
                    ],
                ) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect(
                "expected the closing signal to abort the in-flight handshake and the forwarding task to complete",
            );

            // The handshake was aborted by the closing signal — the counter must NOT have
            // been incremented (it only fires after open_ws_gql_stream returns).
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code_should_not_fail() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: 400: Bad Request"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "This went wrong"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_missing_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_missing_content_type(listener));

        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_invalid_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_invalid_content_type(listener));

        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: subgraph response contains invalid 'content-type' header value \"application/json,application/json\"; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unsupported_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unsupported_content_type(listener));

        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: subgraph response contains unsupported content-type: text/html; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unauthorized() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unauthorized(listener));
        let subgraph_service = with_content_negotiation_layer(SubgraphService::new(
            "test",
            test_http_client_service("test"),
        ));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed: 401: Unauthorized"
        );
    }
}
