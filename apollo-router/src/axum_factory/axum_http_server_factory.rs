//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::middleware::Next;
use axum::response::*;
use axum::routing::get;
use axum::Router;
use futures::channel::oneshot;
use futures::future::join_all;
use futures::prelude::*;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::HeaderValue;
use http::Request;
use http_body::combinators::UnsyncBoxBody;
use hyper::Body;
use itertools::Itertools;
use multimap::MultiMap;
use serde::Serialize;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tower::service_fn;
use tower::BoxError;
use tower::ServiceExt;
use tower_http::trace::TraceLayer;

use super::listeners::ensure_endpoints_consistency;
use super::listeners::ensure_listenaddrs_consistency;
use super::listeners::extra_endpoints;
use super::listeners::ListenersAndRouters;
use super::utils::decompress_request_body;
use super::utils::PropagatingMakeSpan;
use super::ListenAddrAndRouter;
use crate::axum_factory::compression::Compressor;
use crate::axum_factory::listeners::get_extra_listeners;
use crate::axum_factory::listeners::serve_router_on_listen_addr;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::http_server_factory::Listener;
use crate::plugins::traffic_shaping::Elapsed;
use crate::plugins::traffic_shaping::RateLimited;
use crate::router::ApolloRouterError;
use crate::router_factory::Endpoint;
use crate::router_factory::RouterFactory;
use crate::services::router;
use crate::uplink::license_enforcement::LicenseState;
use crate::uplink::license_enforcement::LICENSE_EXPIRED_SHORT_MESSAGE;

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
#[derive(Debug, Default)]
pub(crate) struct AxumHttpServerFactory {
    live: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
}

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self {
            ..Default::default()
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
enum HealthStatus {
    Up,
    Down,
}

#[derive(Debug, Serialize)]
struct Health {
    status: HealthStatus,
}

pub(crate) fn make_axum_router<RF>(
    live: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    service_factory: RF,
    configuration: &Configuration,
    mut endpoints: MultiMap<ListenAddr, Endpoint>,
    license: LicenseState,
) -> Result<ListenersAndRouters, ApolloRouterError>
where
    RF: RouterFactory,
{
    ensure_listenaddrs_consistency(configuration, &endpoints)?;

    if configuration.health_check.enabled {
        tracing::info!(
            "Health check endpoint exposed at {}/health",
            configuration.health_check.listen
        );
        endpoints.insert(
            configuration.health_check.listen.clone(),
            Endpoint::from_router_service(
                "/health".to_string(),
                service_fn(move |req: router::Request| {
                    let mut status_code = StatusCode::OK;
                    let health = if let Some(query) = req.router_request.uri().query() {
                        let query_upper = query.to_ascii_uppercase();
                        // Could be more precise, but sloppy match is fine for this use case
                        if query_upper.starts_with("READY") {
                            let status = if ready.load(Ordering::SeqCst) {
                                HealthStatus::Up
                            } else {
                                // It's hard to get k8s to parse payloads. Especially since we
                                // can't install curl or jq into our docker images because of CVEs.
                                // So, compromise, k8s will interpret this as probe fail.
                                status_code = StatusCode::SERVICE_UNAVAILABLE;
                                HealthStatus::Down
                            };
                            Health { status }
                        } else if query_upper.starts_with("LIVE") {
                            let status = if live.load(Ordering::SeqCst) {
                                HealthStatus::Up
                            } else {
                                // It's hard to get k8s to parse payloads. Especially since we
                                // can't install curl or jq into our docker images because of CVEs.
                                // So, compromise, k8s will interpret this as probe fail.
                                status_code = StatusCode::SERVICE_UNAVAILABLE;
                                HealthStatus::Down
                            };
                            Health { status }
                        } else {
                            Health {
                                status: HealthStatus::Up,
                            }
                        }
                    } else {
                        Health {
                            status: HealthStatus::Up,
                        }
                    };
                    tracing::trace!(?health, request = ?req.router_request, "health check");
                    async move {
                        Ok(router::Response {
                            response: http::Response::builder()
                                .status(status_code)
                                .body::<hyper::Body>(
                                    serde_json::to_vec(&health).map_err(BoxError::from)?.into(),
                                )?,
                            context: req.context,
                        })
                    }
                })
                .boxed(),
            ),
        );
    }

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
        license: LicenseState,
        all_connections_stopped_sender: mpsc::Sender<()>,
    ) -> Self::Future
    where
        RF: RouterFactory,
    {
        let live = self.live.clone();
        let ready = self.ready.clone();
        Box::pin(async move {
            let all_routers = make_axum_router(
                live.clone(),
                ready.clone(),
                service_factory,
                &configuration,
                extra_endpoints,
                license,
            )?;

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
                main_listener,
                actual_main_listen_address.clone(),
                all_routers.main.1,
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
                            listener,
                            listen_addr.clone(),
                            router,
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

    fn live(&self, live: bool) {
        self.live.store(live, Ordering::SeqCst);
    }

    fn ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }
}

fn main_endpoint<RF>(
    service_factory: RF,
    configuration: &Configuration,
    endpoints_on_main_listener: Vec<Endpoint>,
    license: LicenseState,
) -> Result<ListenAddrAndRouter, ApolloRouterError>
where
    RF: RouterFactory,
{
    let cors = configuration.cors.clone().into_layer().map_err(|e| {
        ApolloRouterError::ServiceCreationError(format!("CORS configuration error: {e}").into())
    })?;

    let main_route = main_router::<RF>(configuration)
        .layer(middleware::from_fn(decompress_request_body))
        .layer(middleware::from_fn_with_state(
            (license, Instant::now(), Arc::new(AtomicU64::new(0))),
            license_handler,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(PropagatingMakeSpan { license }))
        .layer(Extension(service_factory))
        .layer(cors);

    let route = endpoints_on_main_listener
        .into_iter()
        .fold(main_route, |acc, r| acc.merge(r.into_router()));

    let listener = configuration.supergraph.listen.clone();
    Ok(ListenAddrAndRouter(listener, route))
}

async fn license_handler<B>(
    State((license, start, delta)): State<(LicenseState, Instant, Arc<AtomicU64>)>,
    request: Request<B>,
    next: Next<B>,
) -> Response {
    if matches!(
        license,
        LicenseState::LicensedHalt | LicenseState::LicensedWarn
    ) {
        ::tracing::error!(
           monotonic_counter.apollo_router_http_requests_total = 1u64,
           status = %500u16,
           error = LICENSE_EXPIRED_SHORT_MESSAGE,
        );

        // This will rate limit logs about license to 1 a second.
        // The way it works is storing the delta in seconds from a starting instant.
        // If the delta is over one second from the last time we logged then try and do a compare_exchange and if successfull log.
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
            ::tracing::error!("{}", LICENSE_EXPIRED_SHORT_MESSAGE);
        }
    }

    if matches!(license, LicenseState::LicensedHalt) {
        http::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(UnsyncBoxBody::default())
            .expect("canned response must be valid")
    } else {
        next.run(request).await
    }
}

pub(super) fn main_router<RF>(configuration: &Configuration) -> axum::Router
where
    RF: RouterFactory,
{
    let mut router = Router::new().route(
        &configuration.supergraph.sanitized_path(),
        get({
            move |Extension(service): Extension<RF>, request: Request<Body>| {
                handle_graphql(service.create().boxed(), request)
            }
        })
        .post({
            move |Extension(service): Extension<RF>, request: Request<Body>| {
                handle_graphql(service.create().boxed(), request)
            }
        }),
    );

    if configuration.supergraph.path == "/*" {
        router = router.route(
            "/",
            get({
                move |Extension(service): Extension<RF>, request: Request<Body>| {
                    handle_graphql(service.create().boxed(), request)
                }
            })
            .post({
                move |Extension(service): Extension<RF>, request: Request<Body>| {
                    handle_graphql(service.create().boxed(), request)
                }
            }),
        );
    }

    router
}

async fn handle_graphql(
    service: router::BoxService,
    http_request: Request<Body>,
) -> impl IntoResponse {
    tracing::info!(counter.apollo_router_session_count_active = 1i64,);

    let request: router::Request = http_request.into();
    let context = request.context.clone();
    let accept_encoding = request
        .router_request
        .headers()
        .get(ACCEPT_ENCODING)
        .cloned();

    let res = service.oneshot(request).await;
    let dur = context.busy_time();
    let processing_seconds = dur.as_secs_f64();

    tracing::info!(histogram.apollo_router_processing_time = processing_seconds,);

    match res {
        Err(e) => {
            tracing::info!(counter.apollo_router_session_count_active = -1i64,);
            if let Some(source_err) = e.source() {
                if source_err.is::<RateLimited>() {
                    return RateLimited::new().into_response();
                }
                if source_err.is::<Elapsed>() {
                    return Elapsed::new().into_response();
                }
            }
            if e.is::<RateLimited>() {
                return RateLimited::new().into_response();
            }
            if e.is::<Elapsed>() {
                return Elapsed::new().into_response();
            }

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "router service call failed",
            )
                .into_response()
        }
        Ok(response) => {
            tracing::info!(counter.apollo_router_session_count_active = -1i64,);
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
                    Body::wrap_stream(compressor.process(body))
                }
            };

            http::Response::from_parts(parts, body).into_response()
        }
    }
}
