//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::fmt::Display;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

use axum::Router;
use axum::extract::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::middleware::Next;
use axum::response::*;
use axum::routing::get;
use futures::channel::oneshot;
use futures::future::join_all;
use futures::prelude::*;
use http::HeaderValue;
use http::Request;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use itertools::Itertools;
use multimap::MultiMap;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tower_http::trace::TraceLayer;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

use super::ENDPOINT_CALLBACK;
use super::ListenAddrAndRouter;
use super::header_size_middleware::HeaderSizeLimitLayer;
use super::listeners::ListenersAndRouters;
use super::listeners::ensure_endpoints_consistency;
use super::listeners::ensure_listenaddrs_consistency;
use super::listeners::extra_endpoints;
use super::utils::PropagatingMakeSpan;
use crate::Context;
use crate::axum_factory::compression::Compressor;
use crate::axum_factory::listeners::get_extra_listeners;
use crate::axum_factory::listeners::serve_router_on_listen_addr;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::graphql;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::http_server_factory::Listener;
use crate::plugins::telemetry::SpanMode;
use crate::router::ApolloRouterError;
use crate::router_factory::Endpoint;
use crate::router_factory::RouterFactory;
use crate::services::router;
use crate::uplink::license_enforcement::APOLLO_ROUTER_LICENSE_EXPIRED;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;
use crate::uplink::license_enforcement::LicenseState;

static BARE_WILDCARD_PATH_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^/\{\*[^/]+\}$").expect("this regex to check wildcard paths is valid")
});

#[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
fn jemalloc_metrics_instruments() -> (
    tokio::task::JoinHandle<()>,
    Vec<opentelemetry::metrics::ObservableGauge<u64>>,
) {
    use crate::axum_factory::metrics::jemalloc;

    (
        jemalloc::start_epoch_advance_loop(),
        vec![
            jemalloc::create_active_gauge(),
            jemalloc::create_allocated_gauge(),
            jemalloc::create_metadata_gauge(),
            jemalloc::create_mapped_gauge(),
            jemalloc::create_resident_gauge(),
            jemalloc::create_retained_gauge(),
        ],
    )
}

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
#[derive(Debug, Default)]
pub(crate) struct AxumHttpServerFactory {}

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

pub(crate) fn make_axum_router<RF>(
    service_factory: RF,
    configuration: &Configuration,
    mut endpoints: MultiMap<ListenAddr, Endpoint>,
    license: Arc<LicenseState>,
) -> Result<ListenersAndRouters, ApolloRouterError>
where
    RF: RouterFactory,
{
    ensure_listenaddrs_consistency(configuration, &endpoints)?;

    ensure_endpoints_consistency(configuration, &endpoints)?;

    let mut main_endpoint = main_endpoint(
        service_factory,
        configuration,
        endpoints
            .remove(&configuration.supergraph.listen)
            .unwrap_or_default(),
        license,
    )?;
    let mut extra_endpoints = extra_endpoints(endpoints);

    // put any extra endpoint that uses the main ListenAddr into the main router
    if let Some(routers) = extra_endpoints.remove(&main_endpoint.0) {
        main_endpoint.1 = routers
            .into_iter()
            .fold(main_endpoint.1, |acc, r| acc.merge(r));
    }

    Ok(ListenersAndRouters {
        main: main_endpoint,
        extra: extra_endpoints,
    })
}

impl HttpServerFactory for AxumHttpServerFactory {
    type Future = Pin<Box<dyn Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send>>;

    fn create<RF>(
        &self,
        service_factory: RF,
        configuration: Arc<Configuration>,
        mut main_listener: Option<Listener>,
        previous_listeners: Vec<(ListenAddr, Listener)>,
        extra_endpoints: MultiMap<ListenAddr, Endpoint>,
        license: Arc<LicenseState>,
        all_connections_stopped_sender: mpsc::Sender<()>,
    ) -> Self::Future
    where
        RF: RouterFactory,
    {
        Box::pin(async move {
            let pipeline_ref = service_factory.pipeline_ref().clone();
            let all_routers =
                make_axum_router(service_factory, &configuration, extra_endpoints, license)?;

            // serve main router

            // if we received a TCP listener, reuse it, otherwise create a new one
            let main_listener = match all_routers.main.0.clone() {
                ListenAddr::SocketAddr(addr) => {
                    let tls_config = configuration
                        .tls
                        .supergraph
                        .as_ref()
                        .map(|tls| tls.tls_config())
                        .transpose()?;
                    let tls_acceptor = tls_config.clone().map(TlsAcceptor::from);

                    match main_listener.take() {
                        Some(Listener::Tcp(listener)) => {
                            if listener.local_addr().ok() == Some(addr) {
                                Listener::new_from_listener(listener, tls_acceptor)
                            } else {
                                Listener::new_from_socket_addr(addr, tls_acceptor).await?
                            }
                        }
                        Some(Listener::Tls { listener, .. }) => {
                            if listener.local_addr().ok() == Some(addr) {
                                Listener::new_from_listener(listener, tls_acceptor)
                            } else {
                                Listener::new_from_socket_addr(addr, tls_acceptor).await?
                            }
                        }
                        _ => Listener::new_from_socket_addr(addr, tls_acceptor).await?,
                    }
                }
                #[cfg(unix)]
                ListenAddr::UnixSocket(path) => {
                    match main_listener.take().and_then(|listener| {
                        listener.local_addr().ok().and_then(|l| {
                            if l == ListenAddr::UnixSocket(path.clone()) {
                                Some(listener)
                            } else {
                                None
                            }
                        })
                    }) {
                        Some(listener) => listener,
                        None => Listener::Unix(
                            UnixListener::bind(path)
                                .map_err(ApolloRouterError::ServerCreationError)?,
                        ),
                    }
                }
            };
            let actual_main_listen_address = main_listener
                .local_addr()
                .map_err(ApolloRouterError::ServerCreationError)?;

            let (main_server, main_shutdown_sender) = serve_router_on_listen_addr(
                pipeline_ref.clone(),
                actual_main_listen_address.clone(),
                main_listener,
                configuration.supergraph.connection_shutdown_timeout,
                all_routers.main.1,
                configuration.server.http.clone(),
                configuration.limits.clone(),
                configuration.limits.http1_max_request_headers,
                configuration.limits.http1_max_request_buf_size,
                all_connections_stopped_sender.clone(),
            );

            tracing::info!(
                "GraphQL endpoint exposed at {}{} 🚀",
                actual_main_listen_address,
                configuration.supergraph.path
            );

            // serve extra routers

            let listeners_and_routers =
                get_extra_listeners(previous_listeners, all_routers.extra).await?;

            let actual_extra_listen_adresses = listeners_and_routers
                .iter()
                .map(|((_, l), _)| l.local_addr().expect("checked above"))
                .collect::<Vec<_>>();

            // TODO: It would be great if we could tracing::debug!()
            // all listen addrs *and* paths we have an endpoint on.
            // I can only do it for listen addrs yet, but hey that's a good start
            if !listeners_and_routers.is_empty() {
                let tracing_endpoints = listeners_and_routers
                    .iter()
                    .map(|((_, l), _)| format!("{}", l.local_addr().expect("checked above")))
                    .join(", ");
                tracing::debug!(%tracing_endpoints, "extra endpoints the router listens to");
            }

            let servers_and_shutdowns =
                listeners_and_routers
                    .into_iter()
                    .map(|((listen_addr, listener), router)| {
                        let (server, shutdown_sender) = serve_router_on_listen_addr(
                            pipeline_ref.clone(),
                            listen_addr.clone(),
                            listener,
                            configuration.supergraph.connection_shutdown_timeout,
                            router,
                            configuration.server.http.clone(),
                            configuration.limits.clone(),
                            configuration.limits.http1_max_request_headers,
                            configuration.limits.http1_max_request_buf_size,
                            all_connections_stopped_sender.clone(),
                        );
                        (
                            server.map(|listener| (listen_addr, listener)),
                            shutdown_sender,
                        )
                    });

            let (servers, shutdowns): (Vec<_>, Vec<_>) = servers_and_shutdowns.unzip();

            // graceful shutdown mechanism:
            // create two shutdown channels. One for the main (GraphQL) server and the other for
            // the extra servers (health, metrics, etc...)
            // We spawn a task for each server which just waits to propagate the message to:
            //  - main
            //  - all extras
            // We have two separate channels because we want to ensure that main is notified
            // separately from all other servers and we wait for main to shutdown before we notify
            // extra servers.
            let (outer_main_shutdown_sender, outer_main_shutdown_receiver) =
                oneshot::channel::<()>();
            tokio::task::spawn(async move {
                let _ = outer_main_shutdown_receiver.await;
                if let Err(_err) = main_shutdown_sender.send(()) {
                    tracing::error!("Failed to notify http thread of shutdown");
                }
            });

            let (outer_extra_shutdown_sender, outer_extra_shutdown_receiver) =
                oneshot::channel::<()>();
            tokio::task::spawn(async move {
                let _ = outer_extra_shutdown_receiver.await;
                shutdowns.into_iter().for_each(|sender| {
                    if let Err(_err) = sender.send(()) {
                        tracing::error!("Failed to notify http thread of shutdown")
                    };
                })
            });

            // Spawn the main (GraphQL) server into a task
            let main_future = tokio::task::spawn(main_server)
                .map_err(|_| ApolloRouterError::HttpServerLifecycleError)
                .boxed();

            // Spawn all other servers (health, metrics, etc...) into a task
            let extra_futures = tokio::task::spawn(join_all(servers))
                .map_err(|_| ApolloRouterError::HttpServerLifecycleError)
                .boxed();

            Ok(HttpServerHandle::new(
                outer_main_shutdown_sender,
                outer_extra_shutdown_sender,
                main_future,
                extra_futures,
                Some(actual_main_listen_address),
                actual_extra_listen_adresses,
                all_connections_stopped_sender,
            ))
        })
    }
}

// This function can be removed once https://github.com/apollographql/router/issues/4083 is done.
pub(crate) fn span_mode(configuration: &Configuration) -> SpanMode {
    configuration
        .apollo_plugins
        .plugins
        .iter()
        .find(|(s, _)| s.as_str() == "telemetry")
        .and_then(|(_, v)| v.get("instrumentation").and_then(|v| v.as_object()))
        .and_then(|v| v.get("spans").and_then(|v| v.as_object()))
        .and_then(|v| {
            v.get("mode")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
        })
        .unwrap_or_default()
}

fn main_endpoint<RF>(
    service_factory: RF,
    configuration: &Configuration,
    endpoints_on_main_listener: Vec<Endpoint>,
    license: Arc<LicenseState>,
) -> Result<ListenAddrAndRouter, ApolloRouterError>
where
    RF: RouterFactory,
{
    let cors = configuration.cors.clone().into_layer().map_err(|e| {
        ApolloRouterError::ServiceCreationError(format!("CORS configuration error: {e}").into())
    })?;
    let span_mode = span_mode(configuration);

    // XXX(@goto-bus-stop): in hyper 0.x, we required a HandleErrorLayer around this,
    // to turn errors from decompression into an axum error response. Now,
    // `RequestDecompressionLayer` appears to preserve(?) the error type from the inner service?
    // So maybe we don't need this anymore? But I don't understand what happens to an error *caused
    // by decompression* (such as an invalid compressed data stream).
    let decompression = tower_http::decompression::RequestDecompressionLayer::new()
        .br(true)
        .gzip(true)
        .deflate(true);
    let mut main_route = main_router::<RF>(configuration)
        .layer(decompression)
        .layer(middleware::from_fn_with_state(
            (license.clone(), Instant::now(), Arc::new(AtomicU64::new(0))),
            license_handler,
        ))
        .layer(Extension(service_factory))
        .layer(cors)
        // Telemetry layers MUST be last. This means that they will be hit first during execution of the pipeline
        // Adding layers after telemetry will cause us to lose metrics and spans.
        .layer(
            TraceLayer::new_for_http().make_span_with(PropagatingMakeSpan { license, span_mode }),
        )
        .layer(middleware::from_fn(metrics_handler));

    if let Some(main_endpoint_layer) = ENDPOINT_CALLBACK.get() {
        main_route = main_endpoint_layer(main_route);
    }

    let route = endpoints_on_main_listener
        .into_iter()
        .fold(main_route, |acc, r| {
            let mut router = r.into_router();
            if let Some(main_endpoint_layer) = ENDPOINT_CALLBACK.get() {
                router = main_endpoint_layer(router);
            }

            acc.merge(router)
        });

    let listener = configuration.supergraph.listen.clone();
    Ok(ListenAddrAndRouter(listener, route))
}

async fn metrics_handler(request: Request<axum::body::Body>, next: Next) -> Response {
    let resp = next.run(request).await;
    u64_counter!(
        "apollo.router.operations",
        "The number of graphql operations performed by the Router",
        1,
        "http.response.status_code" = resp.status().as_u16() as i64
    );
    resp
}

async fn license_handler(
    State((license, start, delta)): State<(Arc<LicenseState>, Instant, Arc<AtomicU64>)>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if matches!(
        &*license,
        LicenseState::LicensedHalt { limits: _ } | LicenseState::LicensedWarn { limits: _ }
    ) {
        // This will rate limit logs about license to 1 a second.
        // The way it works is storing the delta in seconds from a starting instant.
        // If the delta is over one second from the last time we logged then try and do a compare_exchange and if successful log.
        // If not successful some other thread will have logged.
        let last_elapsed_seconds = delta.load(Ordering::SeqCst);
        let elapsed_seconds = start.elapsed().as_secs();
        if elapsed_seconds > last_elapsed_seconds
            && delta
                .compare_exchange(
                    last_elapsed_seconds,
                    elapsed_seconds,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        {
            ::tracing::error!(
                code = APOLLO_ROUTER_LICENSE_EXPIRED,
                LICENSE_EXPIRED_SHORT_MESSAGE
            );
        }
    }

    if matches!(&*license, LicenseState::LicensedHalt { limits: _ }) {
        http::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::default())
            .expect("canned response must be valid")
    } else {
        next.run(request).await
    }
}

#[derive(Clone)]
struct HandlerOptions {
    early_cancel: bool,
    experimental_log_on_broken_pipe: bool,
}

pub(super) fn main_router<RF>(configuration: &Configuration) -> axum::Router<()>
where
    RF: RouterFactory,
{
    let mut router = Router::new().route(
        &configuration.supergraph.sanitized_path(),
        get(handle_graphql::<RF>).post(handle_graphql::<RF>),
    );

    if BARE_WILDCARD_PATH_REGEX.is_match(configuration.supergraph.path.as_str()) {
        router = router.route("/", get(handle_graphql::<RF>).post(handle_graphql::<RF>));
    }

    router = router.route_layer(Extension(HandlerOptions {
        early_cancel: configuration.supergraph.early_cancel,
        experimental_log_on_broken_pipe: configuration.supergraph.experimental_log_on_broken_pipe,
    }));

    // Add header size limit middleware
    if let Some(max_header_size) = configuration.limits.http_max_header_size {
        tracing::debug!(?max_header_size, "Adding header size limit middleware");
        router = router.layer(HeaderSizeLimitLayer::new(Some(max_header_size)));
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix))]
    {
        use tower::layer::layer_fn;
        let (_epoch_advance_loop, jemalloc_instrument) = jemalloc_metrics_instruments();
        // Tie the lifetime of the jemalloc instruments to the lifetime of the router
        // by referencing them in a no-op layer.
        router = router.layer(layer_fn(move |service| {
            let _jemalloc_instrument = &jemalloc_instrument;
            service
        }));
    }

    router
}

async fn handle_graphql<RF: RouterFactory>(
    Extension(options): Extension<HandlerOptions>,
    Extension(service_factory): Extension<RF>,
    http_request: Request<axum::body::Body>,
) -> impl IntoResponse {
    let _guard = i64_up_down_counter_with_unit!(
        "apollo.router.session.count.active",
        "Amount of in-flight sessions",
        "{session}",
        1
    );

    let HandlerOptions {
        early_cancel,
        experimental_log_on_broken_pipe,
    } = options;
    let service = service_factory.create();

    let request: router::Request = http_request.into();
    let context = request.context.clone();
    let accept_encoding = request
        .router_request
        .headers()
        .get(ACCEPT_ENCODING)
        .cloned();

    let res = if early_cancel {
        service.oneshot(request).await
    } else {
        // to make sure we can record request handling when the client closes the connection prematurely,
        // we execute the request in a separate task that will run until we get the first response, which
        // means it went through the entire pipeline at least once (not looking at deferred responses or
        // subscription events). This is a bit wasteful, so to avoid unneeded subgraph calls, we insert in
        // the context a flag to indicate that the request is canceled and subgraph calls should not be made
        let mut cancel_handler = CancelHandler::new(&context, experimental_log_on_broken_pipe);
        let task = service
            .oneshot(request)
            .with_current_subscriber()
            .in_current_span();
        let res = match tokio::task::spawn(task).await {
            Ok(res) => res,
            Err(err) => return internal_server_error(err),
        };
        cancel_handler.on_response();
        res
    };

    match res {
        Err(err) => internal_server_error(err),
        Ok(response) => {
            let (mut parts, body) = response.response.into_parts();

            let opt_compressor = accept_encoding
                .as_ref()
                .and_then(|value| value.to_str().ok())
                .and_then(|v| Compressor::new(v.split(',').map(|s| s.trim())));
            let body = match opt_compressor {
                None => body,
                Some(compressor) => {
                    parts.headers.insert(
                        CONTENT_ENCODING,
                        HeaderValue::from_static(compressor.content_encoding()),
                    );
                    router::body::from_result_stream(compressor.process(body))
                }
            };

            http::Response::from_parts(parts, body).into_response()
        }
    }
}

fn internal_server_error<T>(err: T) -> Response
where
    T: Display,
{
    tracing::error!(
        code = "INTERNAL_SERVER_ERROR",
        %err,
    );

    // This intentionally doesn't include an error message as this could represent leakage of internal information.
    // The error message is logged above.
    let error = graphql::Error::builder()
        .message("internal server error")
        .extension_code("INTERNAL_SERVER_ERROR")
        .build();

    let response = graphql::Response::builder().error(error).build();

    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!(response))).into_response()
}

struct CancelHandler<'a> {
    context: &'a Context,
    got_first_response: bool,
    experimental_log_on_broken_pipe: bool,
    span: tracing::Span,
}

impl<'a> CancelHandler<'a> {
    fn new(context: &'a Context, experimental_log_on_broken_pipe: bool) -> Self {
        CancelHandler {
            context,
            got_first_response: false,
            experimental_log_on_broken_pipe,
            span: tracing::Span::current(),
        }
    }

    fn on_response(&mut self) {
        self.got_first_response = true;
    }
}

impl Drop for CancelHandler<'_> {
    fn drop(&mut self) {
        if !self.got_first_response {
            if self.experimental_log_on_broken_pipe {
                self.span
                    .in_scope(|| tracing::error!("broken pipe: the client closed the connection"));
            }
            self.context
                .extensions()
                .with_lock(|lock| lock.insert(CanceledRequest));
        }
    }
}

pub(crate) struct CanceledRequest;

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use tower::Service;

    use super::*;
    use crate::assert_snapshot_subscriber;
    #[test]
    fn test_span_mode_default() {
        let config =
            Configuration::from_str(include_str!("testdata/span_mode_default.router.yaml"))
                .unwrap();
        let mode = span_mode(&config);
        assert_eq!(mode, SpanMode::SpecCompliant);
    }

    #[test]
    fn test_span_mode_spec_compliant() {
        let config = Configuration::from_str(include_str!(
            "testdata/span_mode_spec_compliant.router.yaml"
        ))
        .unwrap();
        let mode = span_mode(&config);
        assert_eq!(mode, SpanMode::SpecCompliant);
    }

    #[test]
    fn test_span_mode_deprecated() {
        let config =
            Configuration::from_str(include_str!("testdata/span_mode_deprecated.router.yaml"))
                .unwrap();
        let mode = span_mode(&config);
        assert_eq!(mode, SpanMode::Deprecated);
    }

    // Perform a short wait, (100ns) which is intended to complete before the http router call. If
    // it does complete first, then the http router call will be cancelled and we'll see an error
    // log in our assert.
    #[tokio::test(flavor = "multi_thread")]
    async fn request_cancel_log() {
        let mut http_router = crate::TestHarness::builder()
            .configuration_yaml(include_str!("testdata/log_on_broken_pipe.router.yaml"))
            .expect("invalid configuration")
            .schema(include_str!("../testdata/supergraph.graphql"))
            .build_http_service()
            .await
            .unwrap();

        async {
            let _res = tokio::time::timeout(
                std::time::Duration::from_nanos(100),
                http_router.call(
                    http::Request::builder()
                        .method("POST")
                        .uri("/")
                        .header(ACCEPT, "application/json")
                        .header(CONTENT_TYPE, "application/json")
                        .body(router::body::from_bytes(
                            r#"{"query":"query { me { name }}"}"#,
                        ))
                        .unwrap(),
                ),
            )
            .await;
        }
        .with_subscriber(assert_snapshot_subscriber!(
            tracing_core::LevelFilter::ERROR
        ))
        .await
    }

    // Perform a short wait, (100ns) which is intended to complete before the http router call. If
    // it does complete first, then the http router call will be cancelled and we'll not see an
    // error log in our assert.
    #[tokio::test(flavor = "multi_thread")]
    async fn request_cancel_no_log() {
        let mut http_router = crate::TestHarness::builder()
            .configuration_yaml(include_str!("testdata/no_log_on_broken_pipe.router.yaml"))
            .expect("invalid configuration")
            .schema(include_str!("../testdata/supergraph.graphql"))
            .build_http_service()
            .await
            .unwrap();

        async {
            let _res = tokio::time::timeout(
                std::time::Duration::from_nanos(100),
                http_router.call(
                    http::Request::builder()
                        .method("POST")
                        .uri("/")
                        .header(ACCEPT, "application/json")
                        .header(CONTENT_TYPE, "application/json")
                        .body(router::body::from_bytes(
                            r#"{"query":"query { me { name }}"}"#,
                        ))
                        .unwrap(),
                ),
            )
            .await;
        }
        .with_subscriber(assert_snapshot_subscriber!(
            tracing_core::LevelFilter::ERROR
        ))
        .await
    }
}
