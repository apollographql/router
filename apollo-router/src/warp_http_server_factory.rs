use crate::configuration::{Configuration, Cors, ListenAddr};
use crate::http_server_factory::{HttpServerFactory, HttpServerHandle, Listener, NetworkStream};
use crate::FederatedServerError;
use apollo_router_core::http_compat::{Request, RequestBuilder, Response};
use apollo_router_core::prelude::*;
use apollo_router_core::ResponseBody;
use bytes::Bytes;
use futures::{channel::oneshot, prelude::*};
use http::uri::Authority;
use hyper::server::conn::Http;
use once_cell::sync::Lazy;
use opentelemetry::propagation::Extractor;
use reqwest::Url;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::Notify;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tower_service::Service;
use tracing::{Level, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use warp::{
    http::{header::HeaderMap, StatusCode},
    Filter,
};
use warp::{Rejection, Reply};

/// A basic http server using warp.
/// Uses streaming as primary method of response.
/// Redirects to studio for GET requests.
#[derive(Debug)]
pub(crate) struct WarpHttpServerFactory;

impl WarpHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl HttpServerFactory for WarpHttpServerFactory {
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>;

    fn create<RS>(
        &self,
        service: RS,
        configuration: Arc<Configuration>,
        listener: Option<Listener>,
    ) -> Self::Future
    where
        RS: Service<Request<graphql::Request>, Response = Response<ResponseBody>, Error = BoxError>
            + Send
            + Sync
            + Clone
            + 'static,

        <RS as Service<Request<apollo_router_core::Request>>>::Future: std::marker::Send,
    {
        Box::pin(async move {
            let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
            let listen_address = configuration.server.listen.clone();

            let cors = configuration
                .server
                .cors
                .as_ref()
                .map(|cors_configuration| cors_configuration.into_warp_middleware())
                .unwrap_or_else(|| Cors::builder().build().into_warp_middleware());

            let routes = get_health_request()
                .or(get_graphql_request_or_redirect(service.clone()))
                .or(post_graphql_request(service.clone()))
                .with(cors);

            // generate a hyper service from warp routes
            let svc = warp::service(routes);

            let svc = ServiceBuilder::new()
                // generate a tracing span that covers request parsing and response serializing
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().level(Level::INFO)),
                )
                .service(svc);

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
                            let svc = svc.clone();
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
                                                stream
                                                    .set_nodelay(true)
                                                    .expect(
                                                        "this should not fail unless the socket is invalid",
                                                    );
                                                    let connection = Http::new()
                                                    .http1_keep_alive(true)
                                                    .serve_connection(stream, svc);

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
                                                let connection = Http::new()
                                                .http1_keep_alive(true)
                                                .serve_connection(stream, svc);

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

fn get_graphql_request_or_redirect<RS>(
    service: RS,
) -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone
where
    RS: Service<Request<graphql::Request>, Response = Response<ResponseBody>, Error = BoxError>
        + Send
        + Clone
        + 'static,
    <RS as Service<Request<apollo_router_core::Request>>>::Future: std::marker::Send,
{
    warp::get()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::header::optional::<String>("accept"))
        .and(
            warp::query::raw()
                .or(warp::any().map(String::default))
                .unify(),
        )
        .and(warp::header::headers_cloned())
        .and(warp::host::optional())
        .and_then(
            move |accept: Option<String>,
                  query: String,
                  header_map: HeaderMap,
                  authority: Option<Authority>| {
                let service = service.clone();
                async move {
                    let reply: Box<dyn Reply> = if accept.map(prefers_html).unwrap_or_default() {
                        display_home_page()
                    } else if let Ok(request) = graphql::Request::from_urlencoded_query(query) {
                        run_graphql_request(
                            service,
                            authority,
                            http::Method::GET,
                            request,
                            header_map,
                        )
                        .await
                    } else {
                        Box::new(warp::reply::with_status(
                            "Invalid GraphQL request",
                            StatusCode::BAD_REQUEST,
                        ))
                    };

                    Ok::<_, warp::reject::Rejection>(reply)
                }
            },
        )
}

fn display_home_page() -> Box<dyn Reply> {
    let html = include_str!("../resources/index.html");
    Box::new(warp::reply::html(html))
}

fn get_health_request() -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone {
    warp::get()
        .and(warp::path(".well-known"))
        .and(warp::path("apollo"))
        .and(warp::path("server-health"))
        .and_then(move || async {
            static RESULT: Lazy<serde_json::Value> =
                Lazy::new(|| serde_json::json!({"status": "pass"}));

            let reply = Box::new(warp::reply::json(&*RESULT)) as Box<dyn Reply>;
            Ok::<_, Rejection>(reply)
        })
}

fn post_graphql_request<RS>(
    service: RS,
) -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone
where
    RS: Service<Request<graphql::Request>, Response = Response<ResponseBody>, Error = BoxError>
        + Send
        + Clone
        + 'static,
    <RS as Service<Request<apollo_router_core::Request>>>::Future: std::marker::Send,
{
    warp::post()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::body::json())
        .and(warp::header::headers_cloned())
        .and(warp::host::optional())
        .and_then(
            move |request: graphql::Request,
                  header_map: HeaderMap,
                  authority: Option<Authority>| {
                let service = service.clone();
                async move {
                    let reply = run_graphql_request(
                        service,
                        authority,
                        http::Method::POST,
                        request,
                        header_map,
                    )
                    .await;
                    Ok::<_, warp::reject::Rejection>(reply)
                }
            },
        )
}

// graphql_request is traced at the info level so that it can be processed normally in apollo telemetry.
#[tracing::instrument(skip_all,
    level = "info"
    name = "graphql_request",
    fields(
        query = %request.query.clone().unwrap_or_default(),
        operation_name = %request.operation_name.clone().unwrap_or_else(|| "".to_string()),
        client_name,
        client_version
    )
)]
fn run_graphql_request<RS>(
    service: RS,
    authority: Option<Authority>,
    method: http::Method,
    request: graphql::Request,
    header_map: HeaderMap,
) -> impl Future<Output = Box<dyn Reply>> + Send
where
    RS: Service<Request<graphql::Request>, Response = Response<ResponseBody>, Error = BoxError>
        + Send
        + Clone
        + 'static,
    <RS as Service<Request<apollo_router_core::Request>>>::Future: std::marker::Send,
{
    if let Some(client_name) = header_map.get("apollographql-client-name") {
        // Record the client name as part of the current span
        Span::current().record("client_name", &client_name.to_str().unwrap_or_default());
    }
    if let Some(client_version) = header_map.get("apollographql-client-version") {
        // Record the client version as part of the current span
        Span::current().record(
            "client_version",
            &client_version.to_str().unwrap_or_default(),
        );
    }

    // retrieve and reuse the potential trace id from the caller
    opentelemetry::global::get_text_map_propagator(|injector| {
        injector.extract_with_context(&Span::current().context(), &HeaderMapCarrier(&header_map));
    });

    async move {
        match service.ready_oneshot().await {
            Ok(mut service) => {
                let uri = match authority {
                    Some(authority) => Url::parse(&format!("http://{}", authority.as_str()))
                        .expect("if the authority is some then the URL is valid; qed"),
                    None => Url::parse("http://router").unwrap(),
                };

                let mut http_request = RequestBuilder::new(method, uri).body(request).unwrap();
                *http_request.headers_mut() = header_map;

                let response = service
                    .call(http_request)
                    .await
                    .map(|response| {
                        tracing::trace_span!("serialize_response")
                            .in_scope(|| {
                                response.map(|body| {
                                    Bytes::from(
                                        serde_json::to_vec(&body)
                                            .expect("responsebody is serializable; qed"),
                                    )
                                })
                            })
                            .into()
                    })
                    .unwrap_or_else(|e| {
                        tracing::error!("router serivce call failed: {}", e);
                        http::Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Bytes::from_static(b"router service call failed"))
                            .expect("static response building cannot fail; qed")
                    });

                Box::new(response) as Box<dyn Reply>
            }
            Err(e) => {
                tracing::error!("router service is not available to process request: {}", e);
                Box::new(warp::reply::with_status(
                    "router service is not available to process request",
                    StatusCode::SERVICE_UNAVAILABLE,
                ))
            }
        }
    }
}

fn prefers_html(accept_header: String) -> bool {
    accept_header
        .split(',')
        .map(|a| a.trim())
        .any(|a| a == "text/html")
}

struct HeaderMapCarrier<'a>(&'a HeaderMap);

impl<'a> Extractor for HeaderMapCarrier<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        if let Some(value) = self.0.get(key).and_then(|x| x.to_str().ok()) {
            tracing::trace!(
                "found OpenTelemetry key in user's request: {}={}",
                key,
                value
            );
            Some(value)
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|x| x.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::Cors;
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
            fn service_call(&mut self, req: Request<graphql::Request>) -> Result<Response<ResponseBody>, BoxError>;
        }
    }

    async fn init(mut mock: MockRouterService) -> (HttpServerHandle, Client) {
        let server_factory = WarpHttpServerFactory::new();
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
            )
            .await
            .expect("Failed to create server factory");
        let client = reqwest::Client::builder()
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
        let server_factory = WarpHttpServerFactory::new();
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
            )
            .await
            .expect("Failed to create server factory");

        server
    }

    #[test(tokio::test)]
    async fn display_home_page() -> Result<(), FederatedServerError> {
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
            assert!(response
                .text()
                .await
                .unwrap()
                .starts_with("<!DOCTYPE html>"))
        }

        server.shutdown().await
    }

    #[test(tokio::test)]
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

    #[test(tokio::test)]
    async fn response() -> Result<(), FederatedServerError> {
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

        server.shutdown().await
    }

    #[test(tokio::test)]
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
                .to_response(true);
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
            .to_response(true)
        );
        server.shutdown().await
    }

    #[test(tokio::test)]
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

    #[test(tokio::test)]
    async fn test_health_check() {
        let filter = get_health_request();

        let res = warp::test::request()
            .path("/.well-known/apollo/server-health")
            .reply(&filter)
            .await;

        insta::assert_debug_snapshot!("health_check", res);
    }

    #[test(tokio::test)]
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

        let output =
            send_to_unix_socket(server.listen_address(), "POST", r#"{"query":"query"}"#).await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        // Get query
        let output =
            send_to_unix_socket(server.listen_address(), "GET", r#"{"query":"query"}"#).await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        server.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    async fn send_to_unix_socket(addr: &ListenAddr, method: &str, body: &str) -> Vec<u8> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Interest};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(addr.to_string()).await.unwrap();
        stream.ready(Interest::WRITABLE).await.unwrap();
        stream
            .write_all(
                format!(
                    "{} / HTTP/1.1\r
Host: localhost:4100\r
Content-Length: {}\r

{}\n",
                    method,
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
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
}
