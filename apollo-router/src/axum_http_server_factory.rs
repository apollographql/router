//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_compression::tokio::write::BrotliDecoder;
use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::ZlibDecoder;
use axum::body::StreamBody;
use axum::extract::Extension;
use axum::extract::Host;
use axum::extract::OriginalUri;
use axum::http::header::HeaderMap;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::middleware::{self};
use axum::response::*;
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use futures::channel::oneshot;
use futures::future::ready;
use futures::prelude::*;
use futures::stream::once;
use futures::stream::BoxStream;
use futures::StreamExt;
use http::header::CONTENT_ENCODING;
use http::HeaderValue;
use http::Request;
use http::Uri;
use hyper::server::conn::Http;
use hyper::Body;
use opentelemetry::global;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::TraceContextExt;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::Notify;
use tower::util::BoxService;
use tower::BoxError;
use tower::MakeService;
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;
use tower_http::trace::MakeSpan;
use tower_http::trace::TraceLayer;
use tower_service::Service;
use tracing::Level;
use tracing::Span;

use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::graphql;
use crate::http_ext;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::http_server_factory::Listener;
use crate::http_server_factory::NetworkStream;
use crate::plugin::Handler;
use crate::router::ApolloRouterError;
use crate::router_factory::RouterServiceFactory;

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

impl HttpServerFactory for AxumHttpServerFactory {
    type Future = Pin<Box<dyn Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send>>;

    fn create<RF>(
        &self,
        service_factory: RF,
        configuration: Arc<Configuration>,
        listener: Option<Listener>,
        plugin_handlers: HashMap<String, Handler>,
    ) -> Self::Future
    where
        RF: RouterServiceFactory,
    {
        Box::pin(async move {
            let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
            let listen_address = configuration.server.listen.clone();

            let cors = configuration
                .server
                .cors
                .clone()
                .unwrap_or_default()
                .into_layer()
                .map_err(|e| {
                    ApolloRouterError::ConfigError(
                        crate::configuration::ConfigurationError::LayerConfiguration {
                            layer: "Cors".to_string(),
                            error: e,
                        },
                    )
                })?;
            let graphql_endpoint = if configuration.server.endpoint.ends_with("/*") {
                // Needed for axum (check the axum docs for more information about wildcards https://docs.rs/axum/latest/axum/struct.Router.html#wildcards)
                format!("{}router_extra_path", configuration.server.endpoint)
            } else {
                configuration.server.endpoint.clone()
            };
            let mut router = Router::new()
                .route(
                    &graphql_endpoint,
                    get({
                        let display_landing_page = configuration.server.landing_page;
                        move |host: Host,
                              Extension(service): Extension<RF>,
                              http_request: Request<Body>| {
                            handle_get(
                                host,
                                service.new_service().boxed(),
                                http_request,
                                display_landing_page,
                            )
                        }
                    })
                    .post({
                        move |host: Host,
                              uri: OriginalUri,
                              request: Json<graphql::Request>,
                              Extension(service): Extension<RF>,
                              header_map: HeaderMap| {
                            handle_post(
                                host,
                                uri,
                                request,
                                service.new_service().boxed(),
                                header_map,
                            )
                        }
                    }),
                )
                .layer(middleware::from_fn(decompress_request_body))
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
                .route(&configuration.server.health_check_path, get(health_check))
                .layer(Extension(service_factory))
                .layer(cors)
                .layer(CompressionLayer::new()); // To compress response body

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
                            .map_err(ApolloRouterError::ServerCreationError)?,
                    ),
                    #[cfg(unix)]
                    ListenAddr::UnixSocket(path) => Listener::Unix(
                        UnixListener::bind(path).map_err(ApolloRouterError::ServerCreationError)?,
                    ),
                }
            };
            let actual_listen_address = listener
                .local_addr()
                .map_err(ApolloRouterError::ServerCreationError)?;

            tracing::info!(
                "GraphQL endpoint exposed at {}{} 🚀",
                actual_listen_address,
                configuration.server.endpoint
            );
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
                .map_err(|_| ApolloRouterError::HttpServerLifecycleError)
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
    let req = Request::from_parts(head, body).into();
    handler.oneshot(req).await.map_err(|err| err.to_string())
}

async fn handle_get(
    Host(host): Host,
    service: BoxService<
        http_ext::Request<graphql::Request>,
        http_ext::Response<BoxStream<'static, graphql::Response>>,
        BoxError,
    >,
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
    service: BoxService<
        http_ext::Request<graphql::Request>,
        http_ext::Response<BoxStream<'static, graphql::Response>>,
        BoxError,
    >,
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

async fn run_graphql_request<RS>(
    service: RS,
    http_request: Request<graphql::Request>,
) -> impl IntoResponse
where
    RS: Service<
            http_ext::Request<graphql::Request>,
            Response = http_ext::Response<BoxStream<'static, graphql::Response>>,
            Error = BoxError,
        > + Send,
{
    match service.ready_oneshot().await {
        Ok(mut service) => {
            let (head, body) = http_request.into_parts();

            match service.call(Request::from_parts(head, body).into()).await {
                Err(e) => {
                    tracing::error!("router service call failed: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "router service call failed",
                    )
                        .into_response()
                }
                Ok(response) => {
                    let (mut parts, mut stream) = http::Response::from(response).into_parts();
                    parts.headers.insert(
                        "content-type",
                        HeaderValue::from_static("multipart/mixed;boundary=\"graphql\""),
                    );

                    match stream.next().await {
                        None => {
                            tracing::error!("router service is not available to process request",);
                            (
                                StatusCode::SERVICE_UNAVAILABLE,
                                "router service is not available to process request",
                            )
                                .into_response()
                        }
                        Some(response) => {
                            if response.has_next.unwrap_or(false) {
                                let stream = once(ready(response)).chain(stream);

                                let body = stream
                                    .flat_map(|res| {
                                        once(ready(Bytes::from_static(
                                            b"--graphql\r\ncontent-type: application/json\r\n\r\n",
                                        )))
                                        .chain(once(ready(
                                            serde_json::to_vec(&res).unwrap().into(),
                                        )))
                                        .chain(once(ready(Bytes::from_static(b"\r\n"))))
                                    }).chain(once(ready(Bytes::from_static(b"--graphql--\r\ncontent-type: application/json\r\n\r\n{\"hasNext\":false}"))))
                                    .map(Ok::<_, BoxError>);

                                (parts, StreamBody::new(body)).into_response()
                            } else {
                                tracing::trace_span!("serialize_response").in_scope(|| {
                                    http_ext::Response::from(http::Response::from_parts(
                                        parts, response,
                                    ))
                                    .into_response()
                                })
                            }
                        }
                    }
                }
            }
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

async fn decompress_request_body(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, Response> {
    let (parts, body) = req.into_parts();
    let content_encoding = parts.headers.get(&CONTENT_ENCODING);
    macro_rules! decode_body {
        ($decoder: ident, $error_message: expr) => {{
            let body_bytes = hyper::body::to_bytes(body)
                .map_err(|err| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("cannot read request body: {err}"),
                    )
                        .into_response()
                })
                .await?;
            let mut decoder = $decoder::new(Vec::new());
            decoder.write_all(&body_bytes).await.map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("{}: {err}", $error_message),
                )
                    .into_response()
            })?;
            decoder.shutdown().await.map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("{}: {err}", $error_message),
                )
                    .into_response()
            })?;

            Ok(next
                .run(Request::from_parts(parts, Body::from(decoder.into_inner())))
                .await)
        }};
    }

    match content_encoding {
        Some(content_encoding) => match content_encoding.to_str() {
            Ok(content_encoding_str) => match content_encoding_str {
                "br" => decode_body!(BrotliDecoder, "cannot decompress (brotli) request body"),
                "gzip" => decode_body!(GzipDecoder, "cannot decompress (gzip) request body"),
                "deflate" => decode_body!(ZlibDecoder, "cannot decompress (deflate) request body"),
                "identity" => Ok(next.run(Request::from_parts(parts, body)).await),
                unknown => {
                    tracing::error!("unknown content-encoding header value {:?}", unknown);
                    Err((
                        StatusCode::BAD_REQUEST,
                        format!("unknown content-encoding header value: {unknown:?}"),
                    )
                        .into_response())
                }
            },

            Err(err) => Err((
                StatusCode::BAD_REQUEST,
                format!("cannot read content-encoding header: {err}"),
            )
                .into_response()),
        },
        None => Ok(next.run(Request::from_parts(parts, body)).await),
    }
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
    use std::net::SocketAddr;
    use std::str::FromStr;

    use async_compression::tokio::write::GzipEncoder;
    use http::header::ACCEPT_ENCODING;
    use http::header::CONTENT_TYPE;
    use http::header::{self};
    use mockall::mock;
    use reqwest::header::ACCEPT;
    use reqwest::header::ACCESS_CONTROL_ALLOW_HEADERS;
    use reqwest::header::ACCESS_CONTROL_ALLOW_METHODS;
    use reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN;
    use reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS;
    use reqwest::header::ACCESS_CONTROL_REQUEST_METHOD;
    use reqwest::header::ORIGIN;
    use reqwest::redirect::Policy;
    use reqwest::Client;
    use reqwest::Method;
    use reqwest::StatusCode;
    use serde_json::json;
    use test_log::test;
    use tower::service_fn;

    use super::*;
    use crate::configuration::Cors;
    use crate::http_ext::Request;
    use crate::services::new_service::NewService;

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
            fn service_call(&mut self, req: Request<graphql::Request>) -> Result<http_ext::Response<BoxStream<'static, graphql::Response>>, BoxError>;
        }
    }

    type MockRouterServiceType = tower_test::mock::Mock<
        http_ext::Request<graphql::Request>,
        http_ext::Response<Pin<Box<dyn Stream<Item = graphql::Response> + Send>>>,
    >;

    #[derive(Clone)]
    struct TestRouterServiceFactory {
        inner: MockRouterServiceType,
    }

    impl NewService<Request<graphql::Request>> for TestRouterServiceFactory {
        type Service = MockRouterServiceType;

        fn new_service(&self) -> Self::Service {
            self.inner.clone()
        }
    }

    impl RouterServiceFactory for TestRouterServiceFactory {
        type RouterService = MockRouterServiceType;

        type Future = <<TestRouterServiceFactory as NewService<
            http_ext::Request<graphql::Request>,
        >>::Service as Service<http_ext::Request<graphql::Request>>>::Future;

        fn custom_endpoints(&self) -> HashMap<String, Handler> {
            HashMap::new()
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
                TestRouterServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(
                    Configuration::builder()
                        .server(
                            crate::configuration::Server::builder()
                                .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                                .cors(
                                    Cors::builder()
                                        .origins(vec!["http://studio".to_string()])
                                        .build(),
                                )
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
            .create(
                TestRouterServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(conf),
                None,
                plugin_handlers,
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
                TestRouterServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(
                    Configuration::builder()
                        .server(
                            crate::configuration::Server::builder()
                                .listen(ListenAddr::UnixSocket(temp_dir.as_ref().join("sock")))
                                .cors(
                                    Cors::builder()
                                        .origins(vec!["http://studio".to_string()])
                                        .build(),
                                )
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
    async fn it_display_home_page() -> Result<(), ApolloRouterError> {
        // TODO re-enable after the release
        // test_span::init();
        // let root_span = info_span!("root");
        // {
        // let _guard = root_span.enter();
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;

        // Regular studio redirect
        let response = client
            .get(&format!("{}/", server.listen_address()))
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
        // }
        // insta::assert_json_snapshot!(test_span::get_spans_for_root(
        //     &root_span.id().unwrap(),
        //     &test_span::Filter::new(Level::INFO)
        // ));
        Ok(())
    }

    #[tokio::test]
    async fn it_compress_response_body() -> Result<(), ApolloRouterError> {
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yayyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"})) // Body must be bigger than 32 to be compressed
            .build();
        let example_response = expected_response.clone();
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_req| {
                let example_response = example_response.clone();
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.listen_address());

        // Post query
        let response = client
            .post(url.as_str())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("gzip"))
            .body(json!({ "query": "query" }).to_string())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert_eq!(
            response.headers().get(&CONTENT_ENCODING),
            Some(&HeaderValue::from_static("gzip"))
        );

        // Decompress body
        let body_bytes = response.bytes().await.unwrap();
        let mut decoder = GzipDecoder::new(Vec::new());
        decoder.write_all(&body_bytes.to_vec()).await.unwrap();
        decoder.shutdown().await.unwrap();
        let response = decoder.into_inner();
        let graphql_resp: graphql::Response = serde_json::from_slice(&response).unwrap();
        assert_eq!(graphql_resp, expected_response);

        // Get query
        let response = client
            .get(url.as_str())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("gzip"))
            .query(&json!({ "query": "query" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            response.headers().get(&CONTENT_ENCODING),
            Some(&HeaderValue::from_static("gzip"))
        );

        // Decompress body
        let body_bytes = response.bytes().await.unwrap();
        let mut decoder = GzipDecoder::new(Vec::new());
        decoder.write_all(&body_bytes.to_vec()).await.unwrap();
        decoder.shutdown().await.unwrap();
        let response = decoder.into_inner();
        let graphql_resp: graphql::Response = serde_json::from_slice(&response).unwrap();
        assert_eq!(graphql_resp, expected_response);

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn it_decompress_request_body() -> Result<(), ApolloRouterError> {
        let original_body = json!({ "query": "query" });
        let mut encoder = GzipEncoder::new(Vec::new());
        encoder
            .write_all(original_body.to_string().as_bytes())
            .await
            .unwrap();
        encoder.shutdown().await.unwrap();
        let compressed_body = encoder.into_inner();
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yayyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"})) // Body must be bigger than 32 to be compressed
            .build();
        let example_response = expected_response.clone();
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(1)
            .withf(move |req| {
                assert_eq!(req.body().query.as_ref().unwrap(), "query");
                true
            })
            .returning(move |_req| {
                let example_response = example_response.clone();
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.listen_address());

        // Post query
        let response = client
            .post(url.as_str())
            .header(CONTENT_ENCODING, HeaderValue::from_static("gzip"))
            .body(compressed_body.clone())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        assert_eq!(
            response.json::<graphql::Response>().await.unwrap(),
            expected_response,
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_request() -> Result<(), ApolloRouterError> {
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!("{}/", server.listen_address()))
            .body("Garbage")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.shutdown().await
    }

    #[tokio::test]
    async fn response() -> Result<(), ApolloRouterError> {
        // TODO re-enable after the release
        // test_span::init();
        // let root_span = info_span!("root");
        // {
        // let _guard = root_span.enter();
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
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.listen_address());

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
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );

        assert_eq!(
            response.json::<graphql::Response>().await.unwrap(),
            expected_response,
        );

        server.shutdown().await?;
        // }
        // insta::assert_json_snapshot!(test_span::get_spans_for_root(
        //     &root_span.id().unwrap(),
        //     &test_span::Filter::new(Level::INFO)
        // ));
        Ok(())
    }

    #[tokio::test]
    async fn bad_response() -> Result<(), ApolloRouterError> {
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;
        let url = format!("{}/test", server.listen_address());

        // Post query
        let err = client
            .post(url.as_str())
            .body(json!({ "query": "query" }).to_string())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .expect_err("should be not found");

        assert!(err.is_status());
        assert_eq!(err.status(), Some(StatusCode::NOT_FOUND));

        // Get query
        let err = client
            .get(url.as_str())
            .query(&json!({ "query": "query" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .expect_err("should be not found");

        assert!(err.is_status());
        assert_eq!(err.status(), Some(StatusCode::NOT_FOUND));

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn response_with_custom_endpoint() -> Result<(), ApolloRouterError> {
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
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    )
                    .endpoint(String::from("/graphql"))
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
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

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn response_with_custom_prefix_endpoint() -> Result<(), ApolloRouterError> {
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
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    )
                    .endpoint(String::from("/:my_prefix/graphql"))
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
        let url = format!("{}/prefix/graphql", server.listen_address());

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

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn response_with_custom_endpoint_wildcard() -> Result<(), ApolloRouterError> {
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(4)
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    )
                    .endpoint(String::from("/graphql/*"))
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
        for url in &[
            format!("{}/graphql/test", server.listen_address()),
            format!("{}/graphql/anothertest", server.listen_address()),
        ] {
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
        }

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_get_requests() -> Result<(), ApolloRouterError>
    {
        // TODO re-enable after the release
        // test_span::init();
        // let root_span = info_span!("root");
        // {
        // let _guard = root_span.enter();
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
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.listen_address());

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

        server.shutdown().await?;
        // }
        // insta::assert_json_snapshot!(test_span::get_spans_for_root(
        //     &root_span.id().unwrap(),
        //     &test_span::Filter::new(Level::INFO)
        // ));
        Ok(())
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name_on_post_requests() -> Result<(), ApolloRouterError>
    {
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
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.listen_address());

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
    async fn response_failure() -> Result<(), ApolloRouterError> {
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |_| {
                let example_response = crate::error::FetchError::SubrequestHttpError {
                    service: "Mock service".to_string(),
                    reason: "Mock error".to_string(),
                }
                .to_response();
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!("{}/", server.listen_address()))
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
            crate::error::FetchError::SubrequestHttpError {
                service: "Mock service".to_string(),
                reason: "Mock error".to_string(),
            }
            .to_response()
        );
        server.shutdown().await
    }

    #[tokio::test]
    async fn cors_preflight() -> Result<(), ApolloRouterError> {
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;

        let response = client
            .request(Method::OPTIONS, &format!("{}/", server.listen_address()))
            .header(ACCEPT, "text/html")
            .header(ORIGIN, "http://studio")
            .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                ACCESS_CONTROL_REQUEST_HEADERS,
                "Content-type, x-an-other-test-header, apollo-require-preflight",
            )
            .send()
            .await
            .unwrap();

        assert_header!(
            &response,
            ACCESS_CONTROL_ALLOW_ORIGIN,
            vec!["http://studio"],
            "Incorrect access control allow origin header"
        );
        let headers = response.headers().get_all(ACCESS_CONTROL_ALLOW_HEADERS);
        assert_header_contains!(
            &response,
            ACCESS_CONTROL_ALLOW_HEADERS,
            &["Content-type, x-an-other-test-header, apollo-require-preflight"],
            "Incorrect access control allow header header {headers:?}"
        );
        assert_header_contains!(
            &response,
            ACCESS_CONTROL_ALLOW_METHODS,
            &["GET", "POST", "OPTIONS"],
            "Incorrect access control allow methods header"
        );

        assert_eq!(response.status(), StatusCode::OK);

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
                let example_response = example_response.clone();

                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
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
        use tokio::io::AsyncBufReadExt;
        use tokio::io::BufReader;
        use tokio::io::Interest;
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
        // TODO re-enable after the release
        // test_span::init();
        // let root_span = info_span!("root");
        // {
        // let _guard = root_span.enter();
        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;
        let url = format!(
            "{}/.well-known/apollo/server-health",
            server.listen_address()
        );

        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // }
        // insta::assert_json_snapshot!(test_span::get_spans_for_root(
        //     &root_span.id().unwrap(),
        //     &test_span::Filter::new(Level::INFO)
        // ));
    }

    #[tokio::test]
    async fn test_custom_health_check() {
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .health_check_path("/health")
                    .build(),
            )
            .build();
        let expectations = MockRouterService::new();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
        let url = format!("{}/health", server.listen_address());

        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test(tokio::test)]
    async fn it_send_bad_content_type() -> Result<(), ApolloRouterError> {
        let query = "query";
        let operation_name = "operationName";

        let expectations = MockRouterService::new();
        let (server, client) = init(expectations).await;
        let url = format!("{}", server.listen_address());
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
    async fn it_doesnt_display_disabled_home_page() -> Result<(), ApolloRouterError> {
        let expectations = MockRouterService::new();
        let conf = Configuration::builder()
            .server(
                crate::configuration::Server::builder()
                    .listen(SocketAddr::from_str("127.0.0.1:0").unwrap())
                    .cors(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    )
                    .landing_page(false)
                    .build(),
            )
            .build();
        let (server, client) = init_with_config(expectations, conf, HashMap::new()).await;
        let response = client
            .get(&format!("{}/", server.listen_address()))
            .header(ACCEPT, "text/html")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_answers_to_custom_endpoint() -> Result<(), ApolloRouterError> {
        let expectations = MockRouterService::new();
        let plugin_handler = Handler::new(
            service_fn(|req: http_ext::Request<Bytes>| async move {
                Ok::<_, BoxError>(http_ext::Response {
                    inner: http::Response::builder()
                        .status(StatusCode::OK)
                        .body(format!("{} + {}", req.method(), req.uri().path()).into())
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
                    .cors(
                        Cors::builder()
                            .origins(vec!["http://studio".to_string()])
                            .build(),
                    )
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
    async fn it_checks_the_shape_of_router_request() -> Result<(), ApolloRouterError> {
        let mut expectations = MockRouterService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |req| {
                Ok(http_ext::Response::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(
                            graphql::Response::builder()
                                .data(json!(format!(
                                    "{} + {} + {:?}",
                                    req.method(),
                                    req.uri(),
                                    serde_json::to_string(req.body()).unwrap()
                                )))
                                .build(),
                        )
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let query = json!(
        {
          "query": "query",
        });
        let url = format!("{}/", server.listen_address());
        let response = client.get(&url).query(&query).send().await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.text().await.unwrap(),
            serde_json::to_string(&json!({
                "data":
                    format!(
                        "GET + {}?query=query + {:?}",
                        url,
                        serde_json::to_string(&query).unwrap()
                    )
            }))
            .unwrap()
        );
        let response = client
            .post(&url)
            .body(query.to_string())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.text().await.unwrap(),
            serde_json::to_string(&json!({
                "data":
                    format!(
                        "POST + {} + {:?}",
                        url,
                        serde_json::to_string(&query).unwrap()
                    )
            }))
            .unwrap()
        );
        server.shutdown().await
    }
}
