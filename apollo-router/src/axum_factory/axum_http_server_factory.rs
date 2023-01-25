//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::*;
use axum::routing::get;
use axum::Router;
use futures::channel::oneshot;
use futures::future::join;
use futures::future::join_all;
use futures::prelude::*;
use http::Request;
use hyper::Body;
use itertools::Itertools;
use multimap::MultiMap;
use serde::Serialize;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
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
    ) -> Self::Future
    where
        RF: RouterFactory,
    {
        Box::pin(async move {
            let all_routers = make_axum_router(service_factory, &configuration, extra_endpoints)?;

            // serve main router

            // if we received a TCP listener, reuse it, otherwise create a new one
            let main_listener = match all_routers.main.0.clone() {
                ListenAddr::SocketAddr(addr) => {
                    match main_listener.take().and_then(|listener| {
                        listener.local_addr().ok().and_then(|l| {
                            if l == ListenAddr::SocketAddr(addr) {
                                Some(listener)
                            } else {
                                None
                            }
                        })
                    }) {
                        Some(listener) => listener,
                        None => Listener::Tcp(
                            TcpListener::bind(addr)
                                .await
                                .map_err(ApolloRouterError::ServerCreationError)?,
                        ),
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
                        let (server, shutdown_sender) =
                            serve_router_on_listen_addr(listener, listen_addr.clone(), router);
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
            ))
        })
    }
}

fn main_endpoint<RF>(
    service_factory: RF,
    configuration: &Configuration,
    endpoints_on_main_listener: Vec<Endpoint>,
) -> Result<ListenAddrAndRouter, ApolloRouterError>
where
    RF: RouterFactory,
{
    let cors = configuration.cors.clone().into_layer().map_err(|e| {
        ApolloRouterError::ServiceCreationError(format!("CORS configuration error: {e}").into())
    })?;

    let main_route = main_router::<RF>(configuration)
        .layer(middleware::from_fn(decompress_request_body))
        .layer(TraceLayer::new_for_http().make_span_with(PropagatingMakeSpan::default()))
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

pub(super) fn main_router<RF>(configuration: &Configuration) -> axum::Router
where
    RF: RouterFactory,
{
    let mut graphql_configuration = configuration.supergraph.clone();
    if graphql_configuration.path.ends_with("/*") {
        // Needed for axum (check the axum docs for more information about wildcards https://docs.rs/axum/latest/axum/struct.Router.html#wildcards)
        graphql_configuration.path = format!("{}router_extra_path", graphql_configuration.path);
    }

    Router::new().route(
        &graphql_configuration.path,
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
    )
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
            tracing::error!("router service call failed: {}", e);
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
