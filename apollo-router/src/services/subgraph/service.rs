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
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;

use super::http::get_uri_details;
use super::http::http_response_to_graphql_response;
use crate::Notify;
use crate::configuration::SubgraphApq;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::error::FetchError;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::InternalServiceBuilderExt as _;
use crate::layers::ServiceBuilderExt as _;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::plugins::limits::response_size_limit::ResponseSizeLimitError;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventResponse;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphRequestBodySize;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphResponseBodySize;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::http::HttpRequest;
use crate::services::http::service::WireByteCount;
use crate::services::layers::apq::subgraph::SubgraphApqLayer;
use crate::services::layers::content_negotiation::SubgraphContentNegotiationLayer;
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
    ) -> Result<Self, BoxError> {
        let name = service.into();
        Ok(Self {
            inner,
            service: Arc::new(name),
        })
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
        let mut attrs = Vec::with_capacity(5);
        let headers_str = crate::services::header_masking::masked_headers_for_log(
            &context,
            crate::services::header_masking::Direction::Request,
            Some(service_name),
            request.headers(),
        );
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.headers"),
            opentelemetry::Value::String(headers_str.into()),
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
            opentelemetry::Value::String(format!("{:?}", request.body()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("subgraph.name"),
            opentelemetry::Value::String(service_name.to_string().into()),
        ));

        log_event(
            level,
            "subgraph.request",
            attrs,
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

#[derive(Clone)]
pub(crate) struct SubgraphServiceFactory {
    pub(crate) services: Arc<
        HashMap<
            String,
            UnconstrainedBuffer<subgraph::Request, BoxFuture<'static, subgraph::ServiceResult>>,
        >,
    >,
}

impl SubgraphServiceFactory {
    pub(crate) fn new(
        services: Vec<(String, subgraph::BoxCloneService)>,
        plugins: Arc<Plugins>,
        notify: Notify<String, graphql::Response>,
        subscription_config: Option<Arc<SubscriptionConfig>>,
        apq_config: SubgraphConfiguration<SubgraphApq>,
    ) -> Self {
        let mut map = HashMap::with_capacity(services.len());
        for (name, service) in services.into_iter() {
            // We have to do a little dance here to insert the subscription and APQ layers at the
            // right place: *after* all user plugins, but *before* the subgraph service proper.
            let apq_enabled = apq_config.get(&name).enabled;

            // One buffer per named subgraph provides per-subgraph backpressure and is
            // required for correct LoadShed / RateLimit behaviour from traffic-shaping
            // plugins (see ServiceBuilderExt::buffered).
            let service = ServiceBuilder::new()
                .buffered()
                .rust_plugins(plugins.clone(), |plugin, service| {
                    plugin.subgraph_service(&name, service)
                })
                .layer(SubscriptionSubgraphLayer::new(
                    notify.clone(),
                    subscription_config.clone(),
                    Arc::from(name.clone()),
                ))
                .layer(SubgraphApqLayer::new(apq_enabled))
                .layer(SubgraphContentNegotiationLayer::default())
                .service(service);

            map.insert(name, service);
        }

        SubgraphServiceFactory {
            services: Arc::new(map),
        }
    }

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

    use SubgraphRequest;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::extract::WebSocketUpgrade;
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
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::Context;
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
    use crate::plugins::subscription::SubscriptionModeConfig;
    use crate::plugins::subscription::WebSocketConfiguration;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphService;
    use crate::protocols::websocket::ClientMessage;
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::http::HttpClientServiceFactory;
    use crate::services::layers::apq::subgraph::SubgraphApqService;
    use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
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
    /// `SubgraphServiceFactory::new`. Most unit tests construct a `SubgraphService` directly and
    /// call it without going through the factory, which would otherwise skip Accept/Content-Type
    /// header injection entirely.
    fn with_content_negotiation_layer(
        s: SubgraphService,
    ) -> SubgraphContentNegotiationService<SubgraphService> {
        SubgraphContentNegotiationLayer::default().layer(s)
    }

    /// Manually rebuilds the production layer stack (Subscription -> APQ -> SubgraphLayer ->
    /// SubgraphService) for subscriptions tests, which construct a `SubgraphService` directly
    /// instead of going through the `SubgraphServiceFactory`.
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
        let subgraph_service = with_subscription_layer(
            SubgraphService::new("testbis", HttpClientServiceFactory::for_test("testbis"))
                .expect("can create a SubgraphService"),
        );
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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_subscription_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
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
                SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                    .expect("can create a SubgraphService"),
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
            let subgraph_service = with_subscription_layer(
                SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                    .expect("can create a SubgraphService"),
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code_should_not_fail() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(listener));
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
