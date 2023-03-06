// With regards to ELv2 licensing, this entire file is license key functionality

//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::pin::Pin;
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
use futures::future::join;
use futures::future::join_all;
use futures::prelude::*;
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
use tower_http::compression::predicate::NotForContentType;
use tower_http::compression::CompressionLayer;
use tower_http::compression::DefaultPredicate;
use tower_http::compression::Predicate;
use tower_http::trace::TraceLayer;

use super::listeners::ensure_endpoints_consistency;
use super::listeners::ensure_listenaddrs_consistency;
use super::listeners::extra_endpoints;
use super::listeners::ListenersAndRouters;
use super::utils::decompress_request_body;
use super::utils::PropagatingMakeSpan;
use super::ListenAddrAndRouter;
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
use crate::uplink::entitlement::EntitlementState;
use crate::uplink::entitlement::ENTITLEMENT_EXPIRED_SHORT_MESSAGE;

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
#[derive(Debug)]
pub(crate) struct AxumHttpServerFactory;

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self
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
    service_factory: RF,
    configuration: &Configuration,
    mut endpoints: MultiMap<ListenAddr, Endpoint>,
    entitlement: EntitlementState,
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
                    let health = Health {
                        status: HealthStatus::Up,
                    };
                    tracing::trace!(?health, request = ?req.router_request, "health check");
                    async move {
                        Ok(router::Response {
                            response: http::Response::builder().body::<hyper::Body>(
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
        entitlement,
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
        entitlement: EntitlementState,
        all_connections_stopped_sender: mpsc::Sender<()>,
    ) -> Self::Future
    where
        RF: RouterFactory,
    {
        Box::pin(async move {
            let all_routers = make_axum_router(
                service_factory,
                &configuration,
                extra_endpoints,
                entitlement,
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

            let (servers, mut shutdowns): (Vec<_>, Vec<_>) = servers_and_shutdowns.unzip();
            shutdowns.push(main_shutdown_sender);

            // graceful shutdown mechanism:
            // we will fan out to all of the servers once we receive a signal
            let (outer_shutdown_sender, outer_shutdown_receiver) = oneshot::channel::<()>();
            tokio::task::spawn(async move {
                let _ = outer_shutdown_receiver.await;
                shutdowns.into_iter().for_each(|sender| {
                    if let Err(_err) = sender.send(()) {
                        tracing::error!("Failed to notify http thread of shutdown")
                    };
                })
            });

            // Spawn the server into a runtime
            let server_future = tokio::task::spawn(join(main_server, join_all(servers)))
                .map_err(|_| ApolloRouterError::HttpServerLifecycleError)
                .boxed();

            Ok(HttpServerHandle::new(
                outer_shutdown_sender,
                server_future,
                Some(actual_main_listen_address),
                actual_extra_listen_adresses,
                all_connections_stopped_sender,
            ))
        })
    }
}

fn main_endpoint<RF>(
    service_factory: RF,
    configuration: &Configuration,
    endpoints_on_main_listener: Vec<Endpoint>,
    entitlement: EntitlementState,
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
            (entitlement, Instant::now(), Arc::new(AtomicU64::new(0))),
            entitlement_handler,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(PropagatingMakeSpan { entitlement }))
        .layer(Extension(service_factory))
        .layer(cors)
        // Compress the response body, except for multipart responses such as with `@defer`.
        // This is a work-around for https://github.com/apollographql/router/issues/1572
        .layer(CompressionLayer::new().compress_when(
            DefaultPredicate::new().and(NotForContentType::const_new("multipart/")),
        ));

    let route = endpoints_on_main_listener
        .into_iter()
        .fold(main_route, |acc, r| acc.merge(r.into_router()));

    let listener = configuration.supergraph.listen.clone();
    Ok(ListenAddrAndRouter(listener, route))
}

async fn entitlement_handler<B>(
    State((entitlement, start, delta)): State<(EntitlementState, Instant, Arc<AtomicU64>)>,
    request: Request<B>,
    next: Next<B>,
) -> Response {
    if matches!(
        entitlement,
        EntitlementState::EntitledHalt | EntitlementState::EntitledWarn
    ) {
        ::tracing::error!(
           monotonic_counter.apollo_router_http_requests_total = 1u64,
           status = %500u16,
           error = ENTITLEMENT_EXPIRED_SHORT_MESSAGE,
        );

        // This will rate limit logs about entitlement to 1 a second.
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
            ::tracing::error!("{}", ENTITLEMENT_EXPIRED_SHORT_MESSAGE);
        }
    }

    if matches!(entitlement, EntitlementState::EntitledHalt) {
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
    tracing::info!(counter.apollo_router_session_count_active = 1,);

    let request: router::Request = http_request.into();
    let context = request.context.clone();

    let res = service.oneshot(request).await;
    let dur = context.busy_time().await;
    let processing_seconds = dur.as_secs_f64();

    tracing::info!(histogram.apollo_router_processing_time = processing_seconds,);

    match res {
        Err(e) => {
            tracing::info!(counter.apollo_router_session_count_active = -1,);
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
            tracing::info!(counter.apollo_router_session_count_active = -1,);
            response.response.into_response()
        }
    }
}
