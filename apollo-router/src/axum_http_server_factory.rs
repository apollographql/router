//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use crate::configuration::{Configuration, Cors, ListenAddr};
use crate::http_server_factory::{HttpServerFactory, HttpServerHandle, Listener, NetworkStream};
use crate::FederatedServerError;
use apollo_router_core::ResponseBody;
use apollo_router_core::{http_compat, Handler};
use apollo_router_core::{prelude::*, DEFAULT_BUFFER_SIZE};
use axum::extract::{Extension, Host, OriginalUri};
use axum::http::{header::HeaderMap, StatusCode};
use axum::response::*;
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use futures::{channel::oneshot, prelude::*};
use http::{HeaderValue, Request, Uri};
use hyper::server::conn::Http;
use hyper::Body;
use opentelemetry::global;
use opentelemetry::trace::{SpanKind, TraceContextExt};
use serde_json::json;
use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::Notify;
use tower::buffer::Buffer;
use tower::util::BoxService;
use tower::MakeService;
use tower::{BoxError, ServiceExt};
use tower_http::trace::{MakeSpan, TraceLayer};
use tower_service::Service;
use tracing::{Level, Span};

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
/// Redirects to studio for GET requests.
#[derive(Debug)]
pub(crate) struct AxumHttpServerFactory;

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self
    }
}

type BufferedService = Buffer<
    BoxService<
        http_compat::Request<graphql::Request>,
        http_compat::Response<ResponseBody>,
        BoxError,
    >,
    http_compat::Request<graphql::Request>,
>;

impl HttpServerFactory for AxumHttpServerFactory {
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>;

    fn create<RS>(
        &self,
        service: RS,
        configuration: Arc<Configuration>,
        listener: Option<Listener>,
        plugin_handlers: HashMap<String, Handler>,
    ) -> Self::Future
    where
        RS: Service<
                http_compat::Request<graphql::Request>,
                Response = http_compat::Response<ResponseBody>,
                Error = BoxError,
            > + Send
            + Sync
            + Clone
            + 'static,

        <RS as Service<http_compat::Request<apollo_router_core::Request>>>::Future:
            std::marker::Send,
    {
        let boxed_service = Buffer::new(service.boxed(), DEFAULT_BUFFER_SIZE);
        Box::pin(async move {
            let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
            let listen_address = configuration.server.listen.clone();

            let cors = configuration
                .server
                .cors
                .clone()
                .map(|cors_configuration| cors_configuration.into_layer())
                .unwrap_or_else(|| Cors::builder().build().into_layer());

            let mut router = Router::new()
                .route(
                    "/",
                    get({
                        let display_landing_page = configuration.server.landing_page;
                        move |host: Host,
                              service: Extension<BufferedService>,
                              http_request: Request<Body>| {
                            handle_get(host, service, http_request, display_landing_page)
                        }
                    })
                    .post(handle_post),
                )
                .route(
                    "/graphql",
                    get({
                        let display_landing_page = configuration.server.landing_page;
                        move |host: Host,
                              service: Extension<BufferedService>,
                              http_request: Request<Body>| {
                            handle_get(host, service, http_request, display_landing_page)
                        }
                    })
                    .post(handle_post),
                )
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(PropagatingMakeSpan::new())
                        .on_response(|resp: &Response<_>, _duration: Duration, span: &Span| {
                            if resp.status() >= StatusCode::BAD_REQUEST {
                                span.record(
                                    "otel.status_code",
                                    &opentelemetry::trace::StatusCode::Error.as_str(),
                                );
                            } else {
                                span.record(
                                    "otel.status_code",
                                    &opentelemetry::trace::StatusCode::Ok.as_str(),
                                );
                            }
                        }),
                )
                .route("/.well-known/apollo/server-health", get(health_check))
                .layer(Extension(boxed_service))
                .layer(cors);

            for (plugin_name, handler) in plugin_handlers {
                router = router.route(
                    &format!("/plugins/{}/*path", plugin_name),
                    get({
                        let new_handler = handler.clone();
                        move |host: Host, request_parts: Request<Body>| {
                            custom_plugin_handler(host, request_parts, new_handler)
                        }
                    })
                    .post({
                        let new_handler = handler.clone();
                        move |host: Host, request_parts: Request<Body>| {
                            custom_plugin_handler(host, request_parts, new_handler)
                        }
                    }),
                );
            }

            let svc = router.into_make_service();

            // if we received a TCP listener, reuse it, otherwise create a new one
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut listener = if let Some(listener) = listener {
                listener
            } else {
                match listen_address {
                    ListenAddr::SocketAddr(addr) => Listener::Tcp(
                        TcpListener::bind(addr)
                            .await
                            .map_err(FederatedServerError::ServerCreationError)?,
                    ),
                    #[cfg(unix)]
                    ListenAddr::UnixSocket(path) => Listener::Unix(
                        UnixListener::bind(path)
                            .map_err(FederatedServerError::ServerCreationError)?,
                    ),
                }
            };
            let actual_listen_address = listener
                .local_addr()
                .map_err(FederatedServerError::ServerCreationError)?;

            // this server reproduces most of hyper::server::Server's behaviour
            // we select over the stop_listen_receiver channel and the listener's
            // accept future. If the channel received something or the sender
            // was dropped, we stop using the listener and send it back through
            // listener_receiver
            let server = async move {
                tokio::pin!(shutdown_receiver);

                let connection_shutdown = Arc::new(Notify::new());
                let mut max_open_file_warning = None;

                loop {
                    tokio::select! {
                        _ = &mut shutdown_receiver => {
                            break;
                        }
                        res = listener.accept() => {
                            let mut svc = svc.clone();
                            let connection_shutdown = connection_shutdown.clone();

                            match res {
                                Ok(res) => {
                                    if max_open_file_warning.is_some(){
                                        tracing::info!("can accept connections again");
                                        max_open_file_warning = None;
                                    }

                                    tokio::task::spawn(async move{
                                        match res {
                                            NetworkStream::Tcp(stream) => {
                                                // TODO: unwrap?
                                                let app = svc.make_service(&stream).await.unwrap();
                                                stream
                                                    .set_nodelay(true)
                                                    .expect(
                                                        "this should not fail unless the socket is invalid",
                                                    );
                                                    let connection = Http::new()
                                                    .http1_keep_alive(true)
                                                    .serve_connection(stream, app);

                                                tokio::pin!(connection);
                                                tokio::select! {
                                                    // the connection finished first
                                                    _res = &mut connection => {
                                                    }
                                                    // the shutdown receiver was triggered first,
                                                    // so we tell the connection to do a graceful shutdown
                                                    // on the next request, then we wait for it to finish
                                                    _ = connection_shutdown.notified() => {
                                                        let c = connection.as_mut();
                                                        c.graceful_shutdown();

                                                        let _= connection.await;
                                                    }
                                                }
                                            }
                                            #[cfg(unix)]
                                            NetworkStream::Unix(stream) => {
                                                // TODO: unwrap?
                                                let app = svc.make_service(&stream).await.unwrap();
                                                let connection = Http::new()
                                                .http1_keep_alive(true)
                                                .serve_connection(stream, app);

                                                tokio::pin!(connection);
                                                tokio::select! {
                                                    // the connection finished first
                                                    _res = &mut connection => {
                                                    }
                                                    // the shutdown receiver was triggered first,
                                                    // so we tell the connection to do a graceful shutdown
                                                    // on the next request, then we wait for it to finish
                                                    _ = connection_shutdown.notified() => {
                                                        let c = connection.as_mut();
                                                        c.graceful_shutdown();

                                                        let _= connection.await;
                                                    }
                                                }
                                            }
                                        }
                                    });
                                }

                                Err(e) => match e.kind() {
                                    // this is already handled by moi and tokio
                                    //std::io::ErrorKind::WouldBlock => todo!(),

                                    // should be treated as EAGAIN
                                    // https://man7.org/linux/man-pages/man2/accept.2.html
                                    // Linux accept() (and accept4()) passes already-pending network
                                    // errors on the new socket as an error code from accept().  This
                                    // behavior differs from other BSD socket implementations.  For
                                    // reliable operation the application should detect the network
                                    // errors defined for the protocol after accept() and treat them
                                    // like EAGAIN by retrying.  In the case of TCP/IP, these are
                                    // ENETDOWN, EPROTO, ENOPROTOOPT, EHOSTDOWN, ENONET, EHOSTUNREACH,
                                    // EOPNOTSUPP, and ENETUNREACH.
                                    //
                                    // those errors are not supported though: needs the unstable io_error_more feature
                                    // std::io::ErrorKind::NetworkDown => todo!(),
                                    // std::io::ErrorKind::HostUnreachable => todo!(),
                                    // std::io::ErrorKind::NetworkUnreachable => todo!(),

                                    //ECONNABORTED
                                    std::io::ErrorKind::ConnectionAborted|
                                    //EINTR
                                    std::io::ErrorKind::Interrupted|
                                    // EINVAL
                                    std::io::ErrorKind::InvalidInput|
                                    std::io::ErrorKind::PermissionDenied |
                                    std::io::ErrorKind::TimedOut |
                                    std::io::ErrorKind::ConnectionReset|
                                    std::io::ErrorKind::NotConnected => {
                                        // the socket was invalid (maybe timedout waiting in accept queue, or was closed)
                                        // we should ignore that and get to the next one
                                        continue;
                                    }

                                    // EPROTO, EOPNOTSUPP, EBADF, EFAULT, EMFILE, ENOBUFS, ENOMEM, ENOTSOCK
                                    std::io::ErrorKind::Other => {
                                        match e.raw_os_error() {
                                            Some(libc::EMFILE) | Some(libc::ENFILE) => {
                                                match max_open_file_warning {
                                                    None => {
                                                        tracing::error!("reached the max open file limit, cannot accept any new connection");
                                                        max_open_file_warning = Some(Instant::now());
                                                    }
                                                    Some(last) => if Instant::now() - last < Duration::from_secs(60) {
                                                        tracing::error!("still at the max open file limit, cannot accept any new connection");
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }

                                    /* we should ignore the remaining errors as they're not supposed
                                    to happen with the accept() call
                                    std::io::ErrorKind::NotFound => todo!(),
                                    std::io::ErrorKind::AddrInUse => todo!(),
                                    std::io::ErrorKind::AddrNotAvailable => todo!(),
                                    std::io::ErrorKind::BrokenPipe => todo!(),
                                    std::io::ErrorKind::AlreadyExists => todo!(),
                                    std::io::ErrorKind::InvalidData => todo!(),
                                    std::io::ErrorKind::WriteZero => todo!(),

                                    std::io::ErrorKind::Unsupported => todo!(),
                                    std::io::ErrorKind::UnexpectedEof => todo!(),
                                    std::io::ErrorKind::OutOfMemory => todo!(),*/
                                    _ => {
                                        continue;
                                    }

                                }
                            }
                        }
                    }
                }

                // the shutdown receiver was triggered so we break out of
                // the server loop, tell the currently active connections to stop
                // then return the TCP listen socket
                connection_shutdown.notify_waiters();
                listener
            };

            // Spawn the server into a runtime
            let server_future = tokio::task::spawn(server)
                .map_err(|_| FederatedServerError::HttpServerLifecycleError)
                .boxed();

            Ok(HttpServerHandle::new(
                shutdown_sender,
                server_future,
                actual_listen_address,
            ))
        })
    }
}

#[derive(Debug)]
struct CustomRejection {
    #[allow(dead_code)]
    msg: String,
}

async fn custom_plugin_handler(
    Host(host): Host,
    request: Request<Body>,
    handler: Handler,
) -> impl IntoResponse {
    let (mut head, body) = request.into_parts();
    let body = hyper::body::to_bytes(body)
        .await
        .map_err(|err| err.to_string())?;
    head.uri = Uri::from_str(&format!("http://{}{}", host, head.uri))
        .expect("if the authority is some then the URL is valid; qed");
    let req = http_compat::Request::from_parts(head, body);
    let res = handler.oneshot(req).await.map_err(|err| err.to_string())?;

    let is_json = matches!(
        res.body(),
        ResponseBody::GraphQL(_) | ResponseBody::RawJSON(_)
    );

    let mut res = res.map(|body| match body {
        ResponseBody::GraphQL(res) => {
            Bytes::from(serde_json::to_vec(&res).expect("responsebody is serializable; qed"))
        }
        ResponseBody::RawJSON(res) => {
            Bytes::from(serde_json::to_vec(&res).expect("responsebody is serializable; qed"))
        }
        ResponseBody::Text(res) => Bytes::from(res),
    });

    if is_json {
        res.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }

    Ok::<_, String>(res)
}

async fn handle_get(
    Host(host): Host,
    Extension(service): Extension<BufferedService>,
    http_request: Request<Body>,
    display_landing_page: bool,
) -> impl IntoResponse {
    if http_request
        .headers()
        .get(&http::header::ACCEPT)
        .map(prefers_html)
        .unwrap_or_default()
        && display_landing_page
    {
        return display_home_page().into_response();
    }

    if let Some(request) = http_request
        .uri()
        .query()
        .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
    {
        let mut http_request = http_request.map(|_| request);
        *http_request.uri_mut() = Uri::from_str(&format!("http://{}{}", host, http_request.uri()))
            .expect("the URL is already valid because it comes from axum; qed");
        return run_graphql_request(service, http_request)
            .await
            .into_response();
    }

    (StatusCode::BAD_REQUEST, "Invalid Graphql request").into_response()
}

async fn handle_post(
    Host(host): Host,
    OriginalUri(uri): OriginalUri,
    Json(request): Json<graphql::Request>,
    Extension(service): Extension<BufferedService>,
    header_map: HeaderMap,
) -> impl IntoResponse {
    let mut http_request = Request::post(
        Uri::from_str(&format!("http://{}{}", host, uri))
            .expect("the URL is already valid because it comes from axum; qed"),
    )
    .body(request)
    .expect("body has already been parsed; qed");
    *http_request.headers_mut() = header_map;

    run_graphql_request(service, http_request)
        .await
        .into_response()
}

fn display_home_page() -> Html<Bytes> {
    let html = Bytes::from_static(include_bytes!("../resources/index.html"));
    Html(html)
}

async fn health_check() -> impl IntoResponse {
    Json(json!({ "status": "pass" }))
}

async fn run_graphql_request(
    service: Buffer<
        BoxService<
            http_compat::Request<graphql::Request>,
            http_compat::Response<ResponseBody>,
            BoxError,
        >,
        http_compat::Request<graphql::Request>,
    >,
    http_request: Request<graphql::Request>,
) -> impl IntoResponse {
    match service.ready_oneshot().await {
        Ok(mut service) => {
            let (head, body) = http_request.into_parts();

            service
                .call(http_compat::Request::from_parts(head, body))
                .await
                .map(|response| {
                    tracing::trace_span!("serialize_response").in_scope(|| response.into_response())
                })
                .unwrap_or_else(|e| {
                    tracing::error!("router serivce call failed: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "router service call failed",
                    )
                        .into_response()
                })
        }
        Err(e) => {
            tracing::error!("router service is not available to process request: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "router service is not available to process request",
            )
                .into_response()
        }
    }
}

fn prefers_html(accept_header: &HeaderValue) -> bool {
    accept_header
        .to_str()
        .map(|accept_str| {
            accept_str
                .split(',')
                .map(|a| a.trim())
                .any(|a| a == "text/html")
        })
        .unwrap_or_default()
}

#[derive(Clone)]
struct PropagatingMakeSpan;

impl PropagatingMakeSpan {
    fn new() -> Self {
        Self {}
    }
}

impl<B> MakeSpan<B> for PropagatingMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> Span {
        // Before we make the span we need to attach span info that may have come in from the request.
        let context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&opentelemetry_http::HeaderExtractor(request.headers()))
        });

        // If there was no span from the request then it will default to the NOOP span.
        // Attaching the NOOP span has the effect of preventing further tracing.
        if context.span().span_context().is_valid() {
            // We have a valid remote span, attach it to the current thread before creating the root span.
            let _context_guard = context.attach();
            tracing::span!(
                Level::INFO,
                "request",
                method = %request.method(),
                uri = %request.uri(),
                version = ?request.version(),
                "otel.kind" = %SpanKind::Server,
                "otel.status_code" = %opentelemetry::trace::StatusCode::Unset.as_str()
            )
        } else {
            // No remote span, we can go ahead and create the span without context.
            tracing::span!(
                Level::INFO,
                "request",
                method = %request.method(),
                uri = %request.uri(),
                version = ?request.version(),
                "otel.kind" = %SpanKind::Server,
                "otel.status_code" = %opentelemetry::trace::StatusCode::Unset.as_str()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Cors;
    use apollo_router_core::http_compat::Request;
    use http::header::CONTENT_TYPE;
    use mockall::mock;
    use reqwest::header::{
        ACCEPT, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
        ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD,
        ORIGIN,
    };
    use reqwest::redirect::Policy;
    use reqwest::{Client, Method, StatusCode};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use test_log::test;
    use tower::service_fn;
    use tracing::info_span;

    macro_rules! assert_header {
        ($response:expr, $header:expr, $expected:expr $(, $msg:expr)?) => {
            assert_eq!(
                $response
                    .headers()
                    .get_all($header)
                    .iter()
                    .map(|v|v.to_str().unwrap().to_string())
                    .collect::<Vec<_>>(),
                $expected
                $(, $msg)*
            );
        };
    }

    /// `assert_header_contains` works like `assert_headers`,
    /// except it doesn't care for the order of the items
    macro_rules! assert_header_contains {
        ($response:expr, $header:expr, $expected:expr $(, $msg:expr)?) => {
            let header_values = $response
            .headers()
            .get_all($header)
            .iter()
            .map(|v|v.to_str().unwrap().to_string())
            .collect::<Vec<_>>();

            for e in $expected {
                assert!(
                    header_values
                        .iter()
                        .find(|header_value| header_value.contains(&e.to_string()))
                        .is_some(),
                    $($msg)*
                );
            }

        };
    }

    mock! {
        #[derive(Debug)]
        RouterService {
            fn service_call(&mut self, req: Request<graphql::Request>) -> Result<http_compat::Response<ResponseBody>, BoxError>;
        }
    }

    async fn init(mut mock: MockRouterService) -> (HttpServerHandle, Client) {
        let server_factory = AxumHttpServerFactory::new();
        let (service, mut handle) = tower_test::mock::spawn();

        tokio::spawn(async move {
            loop {
                while let Some((request, responder)) = handle.next_request().await {
                    match mock.service_call(request) {
                        Ok(response) => responder.send_response(response),
                        Err(err) => responder.send_error(err),
                    }
                }
            }
        });
        let server = server_factory
            .create(
                service.into_inner(),
                Arc::new(
                    Configuration::builder()
                        .server(
                            crate::configuration::Server::builder()
                                .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                                .cors(Some(
                                    Cors::builder()
                                        .origins(vec!["http://studio".to_string()])
                                        .build(),
                                ))
                                .build(),
                        )
                        .build(),
                ),
                None,
                HashMap::new(),
            )
            .await
            .expect("Failed to create server factory");
        let mut default_headers = HeaderMap::new();
        default_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .redirect(Policy::none())
            .build()
            .unwrap();
        (server, client)
    }

    async fn init_with_config(
        mut mock: MockRouterService,
        conf: Configuration,
        plugin_handlers: HashMap<String, Handler>,
    ) -> (HttpServerHandle, Client) {
        let server_factory = AxumHttpServerFactory::new();
        let (service, mut handle) = tower_test::mock::spawn();

        tokio::spawn(async move {
            loop {
                while let Some((request, responder)) = handle.next_request().await {
                    match mock.service_call(request) {
                        Ok(response) => responder.send_response(response),
                        Err(err) => responder.send_error(err),
                    }
                }
            }
        });
        let server = server_factory
            .create(service.into_inner(), Arc::new(conf), None, plugin_handlers)
            .await
            .expect("Failed to create server factory");
        let mut default_headers = HeaderMap::new();
        default_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .redirect(Policy::none())
            .build()
            .unwrap();
        (server, client)
    }

    #[cfg(unix)]
    async fn init_unix(
        mut mock: MockRouterService,
        temp_dir: &tempfile::TempDir,
    ) -> HttpServerHandle {
        let server_factory = AxumHttpServerFactory::new();
        let (service, mut handle) = tower_test::mock::spawn();

        tokio::spawn(async move {
            loop {
                while let Some((request, responder)) = handle.next_request().await {
                    match mock.service_call(request) {
                        Ok(response) => responder.send_response(response),
                        Err(err) => responder.send_error(err),
                    }
                }
            }
        });
        let server = server_factory
            .create(
                service.into_inner(),
                Arc::new(
                    Configuration::builder()
                        .server(
                            crate::configuration::Server::builder()
                                .listen(ListenAddr::UnixSocket(temp_dir.as_ref().join("sock")))
                                .cors(Some(
                                    Cors::builder()
                                        .origins(vec!["http://studio".to_string()])
                                        .build(),
                                ))
                                .build(),
                        )
                        .build(),
                ),
                None,
                HashMap::new(),
            )
            .await
            .expect("Failed to create server factory");

        server
    }

    #[tokio::test]
    async fn it_display_home_page() -> Result<(), FederatedServerError> {
        test_span::init();
        let root_span = info_span!("root");
        {
            let _guard = root_span.enter();
            let expectations = MockRouterService::new();
            let (server, client) = init(expectations).await;

            for url in vec![
                format!("{}/", server.listen_address()),
                format!("{}/graphql", server.listen_address()),
            ] {
                // Regular studio redirect
                let response = client
                    .get(url.as_str())
                    .header(ACCEPT, "text/html")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    response.text().await.unwrap()
                );
                assert_eq!(response.bytes().await.unwrap(), display_home_page().0);
            }
        }
        insta::assert_json_snapshot!(test_span::get_spans_for_root(
            &root_span.id().unwrap(),
            &test_span::Filter::new(Level::INFO)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn malformed_request() -> Result<(), FederatedServerError> {
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!("{}/graphql", server.listen_address()))
            .body("Garbage")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.shutdown().await
    }

    #[tokio::test]
    async fn response() -> Result<(), FederatedServerError> {
        test_span::init();
        let root_span = info_span!("root");
        {
            let _guard = root_span.enter();
            let expected_response = graphql::Response::builder()
                .data(json!({"response": "yay"}))
                .build();
            let example_response = expected_response.clone();
            let mut expectations = MockRouterService::new();
            expectations
                .expect_service_call()
                .times(2)
                .returning(move |_| {
                    let example_response = example_response.clone();
                    Ok(http::Response::builder()
                        .status(200)
                        .body(ResponseBody::GraphQL(example_response))
                        .unwrap()
                        .into())
                });
            let (server, client) = init(expectations).await;
            let url = format!("{}/graphql", server.listen_address());

            // Post query
            let response = client
                .post(url.as_str())
                .body(json!({ "query": "query" }).to_string())
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();

            assert_eq!(
                response.json::<graphql::Response>().await.unwrap(),
                expected_response,
            );

            // Get query
            let response = client
                .get(url.as_str())
                .query(&json!({ "query": "query" }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();

            assert_eq!(
                response.json::<graphql::Response>().await.unwrap(),
                expected_response,
            );

            server.shutdown().await?
        }
        insta::assert_json_snapshot!(test_span::get_spans_for_root(
            &root_span.id().unwrap(),
            &test_span::Filter::new(Level::INFO)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_get_requests(
    ) -> Result<(), FederatedServerError> {
        test_span::init();
        let root_span = info_span!("root");
        {
            let _guard = root_span.enter();
            let query = "query";
            let expected_query = query;
            let operation_name = "operationName";
            let expected_operation_name = operation_name;

            let expected_response = graphql::Response::builder()
                .data(json!({"response": "yay"}))
                .build();
            let example_response = expected_response.clone();

            let mut expectations = MockRouterService::new();
            expectations
                .expect_service_call()
                .times(1)
                .withf(move |req| {
                    assert_eq!(req.body().query.as_deref().unwrap(), expected_query);
                    assert_eq!(
                        req.body().operation_name.as_deref().unwrap(),
                        expected_operation_name
                    );
                    true
                })
                .returning(move |_| {
                    let example_response = example_response.clone();
                    Ok(http::Response::builder()
                        .status(200)
                        .body(ResponseBody::GraphQL(example_response))
                        .unwrap()
                        .into())
                });
            let (server, client) = init(expectations).await;
            let url = format!("{}/graphql", server.listen_address());

            let response = client
                .get(url.as_str())
                .query(&[("query", query), ("operationName", operation_name)])
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();

            assert_eq!(
                response.json::<graphql::Response>().await.unwrap(),
                expected_response,
            );

            server.shutdown().await?
        }
        insta::assert_json_snapshot!(test_span::get_spans_for_root(
            &root_span.id().unwrap(),
            &test_span::Filter::new(Level::INFO)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_post_requests(
    ) -> Result<(), FederatedServerError> {
        let query = "query";
        let expected_query = query;
        let operation_name = "operationName";
        let expected_operation_name = operation_name;

        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();

        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(1)
            .withf(move |req| {
                assert_eq!(req.body().query.as_deref().unwrap(), expected_query);
                assert_eq!(
                    req.body().operation_name.as_deref().unwrap(),
                    expected_operation_name
                );
                true
            })
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http::Response::builder()
                    .status(200)
                    .body(ResponseBody::GraphQL(example_response))
                    .unwrap()
                    .into())
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/graphql", server.listen_address());

        let response = client
            .post(url.as_str())
            .body(json!({ "query": query, "operationName": operation_name }).to_string())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        assert_eq!(
            response.json::<graphql::Response>().await.unwrap(),
            expected_response,
        );

        server.shutdown().await
    }

    #[tokio::test]
    async fn response_failure() -> Result<(), FederatedServerError> {
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |_| {
                let example_response = graphql::FetchError::SubrequestHttpError {
                    service: "Mock service".to_string(),
                    reason: "Mock error".to_string(),
                }
                .to_response();
                Ok(http::Response::builder()
                    .status(200)
                    .body(ResponseBody::GraphQL(example_response))
                    .unwrap()
                    .into())
            });
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!("{}/graphql", server.listen_address()))
            .body(
                json!(
                {
                  "query": "query",
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap()
            .json::<graphql::Response>()
            .await
            .unwrap();

        assert_eq!(
            response,
            graphql::FetchError::SubrequestHttpError {
                service: "Mock service".to_string(),
                reason: "Mock error".to_string(),
            }
            .to_response()
        );
        server.shutdown().await
    }

    #[tokio::test]
    async fn cors_preflight() -> Result<(), FederatedServerError> {
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;

        for url in vec![
            format!("{}/", server.listen_address()),
            format!("{}/graphql", server.listen_address()),
        ] {
            let response = client
                .request(Method::OPTIONS, &url)
                .header(ACCEPT, "text/html")
                .header(ORIGIN, "http://studio")
                .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(ACCESS_CONTROL_REQUEST_HEADERS, "Content-type")
                .send()
                .await
                .unwrap();

            assert_header!(
                &response,
                ACCESS_CONTROL_ALLOW_ORIGIN,
                vec!["http://studio"],
                "Incorrect access control allow origin header"
            );
            assert_header_contains!(
                &response,
                ACCESS_CONTROL_ALLOW_HEADERS,
                &["content-type"],
                "Incorrect access control allow header header"
            );
            assert_header_contains!(
                &response,
                ACCESS_CONTROL_ALLOW_METHODS,
                &["GET", "POST", "OPTIONS"],
                "Incorrect access control allow methods header"
            );

            assert_eq!(response.status(), StatusCode::OK);
        }

        server.shutdown().await
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn listening_to_unix_socket() {
        let temp_dir = tempfile::tempdir().unwrap();
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();

        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_| {
                Ok(http::Response::builder()
                    .status(200)
                    .body(ResponseBody::GraphQL(example_response.clone()))
                    .unwrap()
                    .into())
            });
        let server = init_unix(expectations, &temp_dir).await;

        let output = send_to_unix_socket(
            server.listen_address(),
            Method::POST,
            r#"{"query":"query"}"#,
        )
        .await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        // Get query
        let output =
            send_to_unix_socket(server.listen_address(), Method::GET, r#"query=query"#).await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        server.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    async fn send_to_unix_socket(addr: &ListenAddr, method: Method, body: &str) -> Vec<u8> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Interest};
        use tokio::net::UnixStream;

        let content = match method {
            Method::GET => {
                format!(
                    "{} /?{} HTTP/1.1\r
Host: localhost:4100\r
Content-Length: {}\r
Content-Type: application/json\r

\n",
                    method.as_str(),
                    body,
                    body.len(),
                )
            }
            Method::POST => {
                format!(
                    "{} / HTTP/1.1\r
Host: localhost:4100\r
Content-Length: {}\r
Content-Type: application/json\r

{}\n",
                    method.as_str(),
                    body.len(),
                    body
                )
            }
            _ => {
                unimplemented!()
            }
        };
        let mut stream = UnixStream::connect(addr.to_string()).await.unwrap();
        stream.ready(Interest::WRITABLE).await.unwrap();
        stream.write_all(content.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let stream = BufReader::new(stream);
        let mut lines = stream.lines();
        let header_first_line = lines
            .next_line()
            .await
            .unwrap()
            .expect("no header received");
        // skip the rest of the headers
        let mut headers = String::new();
        let mut stream = lines.into_inner();
        loop {
            if stream.read_line(&mut headers).await.unwrap() == 2 {
                break;
            }
        }
        // get rest of the buffer as body
        let body = stream.buffer().to_vec();
        assert!(header_first_line.contains(" 200 "), "");
        body
    }

    #[tokio::test]
    async fn test_health_check() {
        test_span::init();
        let root_span = info_span!("root");
        {
            let _guard = root_span.enter();
            let expectations = MockRouterService::new();
            let (server, client) = init(expectations).await;
            let url = format!(
                "{}/.well-known/apollo/server-health",
                server.listen_address()
            );

            let response = client.get(url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        insta::assert_json_snapshot!(test_span::get_spans_for_root(
            &root_span.id().unwrap(),
            &test_span::Filter::new(Level::INFO)
        ));
    }

    #[test(tokio::test)]
    async fn it_send_bad_content_type() -> Result<(), FederatedServerError> {
        let query = "query";
        let operation_name = "operationName";

        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;
        let url = format!("{}/graphql", server.listen_address());
        let response = client
            .post(url.as_str())
            .header(CONTENT_TYPE, "application/yaml")
            .body(json!({ "query": query, "operationName": operation_name }).to_string())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE,);

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_doesnt_display_disabled_home_page() -> Result<(), FederatedServerError> {
        let expectations = MockRouterService::new();
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(Some(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    ))
                    .landing_page(false)
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
        for url in vec![
            format!("{}/", server.listen_address()),
            format!("{}/graphql", server.listen_address()),
        ] {
            let response = client
                .get(url.as_str())
                .header(ACCEPT, "text/html")
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_answers_to_custom_endpoint() -> Result<(), FederatedServerError> {
        let expectations = MockRouterService::new();
        let plugin_handler = Handler::new(
            service_fn(|req: http_compat::Request<Bytes>| async move {
                Ok::<_, BoxError>(http_compat::Response {
                    inner: http::Response::builder()
                        .status(StatusCode::OK)
                        .body(ResponseBody::Text(format!(
                            "{} + {}",
                            req.method(),
                            req.uri().path()
                        )))
                        .unwrap(),
                })
            })
            .boxed(),
        );
        let mut plugin_handlers = HashMap::new();
        plugin_handlers.insert(
            "apollo.test.custom_plugin_with_endpoint".to_string(),
            plugin_handler,
        );

        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(Some(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    ))
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, plugin_handlers).await;

        for path in &["/", "/test"] {
            let response = client
                .get(&format!(
                    "{}/plugins/apollo.test.custom_plugin_with_endpoint{}",
                    server.listen_address(),
                    path
                ))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.text().await.unwrap(),
                format!(
                    "GET + /plugins/apollo.test.custom_plugin_with_endpoint{}",
                    path
                )
            );
        }

        for path in &["/", "/test"] {
            let response = client
                .post(&format!(
                    "{}/plugins/apollo.test.custom_plugin_with_endpoint{}",
                    server.listen_address(),
                    path
                ))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.text().await.unwrap(),
                format!(
                    "POST + /plugins/apollo.test.custom_plugin_with_endpoint{}",
                    path
                )
            );
        }
        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_checks_the_shape_of_router_request() -> Result<(), FederatedServerError> {
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(4)
            .returning(move |req| {
                Ok(http::Response::builder()
                    .status(200)
                    .body(ResponseBody::Text(format!(
                        "{} + {} + {:?}",
                        req.method(),
                        req.uri(),
                        serde_json::to_string(req.body()).unwrap()
                    )))
                    .unwrap()
                    .into())
            });
        let (server, client) = init(expectations).await;
        let query = json!(
        {
          "query": "query",
        });
        for url in vec![
            format!("{}/", server.listen_address()),
            format!("{}/graphql", server.listen_address()),
        ] {
            let response = client.get(&url).query(&query).send().await.unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.text().await.unwrap(),
                serde_json::to_string(&format!(
                    "GET + {}?query=query + {:?}",
                    url,
                    serde_json::to_string(&query).unwrap()
                ))
                .unwrap()
            );
        }
        for url in vec![
            format!("{}/", server.listen_address()),
            format!("{}/graphql", server.listen_address()),
        ] {
            let response = client
                .post(&url)
                .body(query.to_string())
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.text().await.unwrap(),
                serde_json::to_string(&format!(
                    "POST + {} + {:?}",
                    url,
                    serde_json::to_string(&query).unwrap()
                ))
                .unwrap()
            );
        }
        server.shutdown().await
    }
}
