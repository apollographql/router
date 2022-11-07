//! Axum http server factory. Axum provides routing capability on top of Hyper HTTP.
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Extension;
use axum::extract::Host;
use axum::extract::OriginalUri;
use axum::http::header::HeaderMap;
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
use tracing::Span;

use super::handlers::handle_get;
use super::handlers::handle_get_with_static;
use super::handlers::handle_post;
use super::listeners::ensure_endpoints_consistency;
use super::listeners::ensure_listenaddrs_consistency;
use super::listeners::extra_endpoints;
use super::listeners::ListenersAndRouters;
use super::utils::check_accept_header;
use super::utils::decompress_request_body;
use super::utils::PropagatingMakeSpan;
use super::ListenAddrAndRouter;
use crate::axum_factory::listeners::get_extra_listeners;
use crate::axum_factory::listeners::serve_router_on_listen_addr;
use crate::cache::DeduplicatingCache;
use crate::configuration::Configuration;
use crate::configuration::Homepage;
use crate::configuration::ListenAddr;
use crate::configuration::Sandbox;
use crate::graphql;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::http_server_factory::Listener;
use crate::plugins::telemetry::formatters::TRACE_ID_FIELD_NAME;
use crate::router::ApolloRouterError;
use crate::router_factory::Endpoint;
use crate::router_factory::SupergraphServiceFactory;
use crate::services::layers::apq::APQLayer;
use crate::services::transport;
use crate::tracer::TraceId;

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
#[derive(Debug)]
pub(crate) struct AxumHttpServerFactory;

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[derive(Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
enum HealthStatus {
    Up,
    Down,
}

#[derive(Serialize)]
struct Health {
    status: HealthStatus,
}

pub(crate) fn make_axum_router<RF>(
    service_factory: RF,
    configuration: &Configuration,
    mut endpoints: MultiMap<ListenAddr, Endpoint>,
    apq: APQLayer,
) -> Result<ListenersAndRouters, ApolloRouterError>
where
    RF: SupergraphServiceFactory,
{
    ensure_listenaddrs_consistency(configuration, &endpoints)?;

    if configuration.health_check.enabled {
        tracing::info!(
            "healthcheck endpoint exposed at {}/health",
            configuration.health_check.listen
        );
        endpoints.insert(
            configuration.health_check.listen.clone(),
            Endpoint::new(
                "/health".to_string(),
                service_fn(move |_req: transport::Request| {
                    let health = Health {
                        status: HealthStatus::Up,
                    };

                    async move {
                        Ok(http::Response::builder()
                            .body(serde_json::to_vec(&health).map_err(BoxError::from)?.into())?)
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
        apq,
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
        RF: SupergraphServiceFactory,
    {
        Box::pin(async move {
            let apq = APQLayer::with_cache(DeduplicatingCache::new().await);

            let all_routers =
                make_axum_router(service_factory, &configuration, extra_endpoints, apq)?;

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

            let (main_server, main_shutdown_sender) =
                serve_router_on_listen_addr(main_listener, all_routers.main.1);

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
                            serve_router_on_listen_addr(listener, router);
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
    apq: APQLayer,
) -> Result<ListenAddrAndRouter, ApolloRouterError>
where
    RF: SupergraphServiceFactory,
{
    let cors = configuration.cors.clone().into_layer().map_err(|e| {
        ApolloRouterError::ServiceCreationError(format!("CORS configuration error: {e}").into())
    })?;

    let main_route = main_router::<RF>(configuration, apq)
        .layer(middleware::from_fn(decompress_request_body))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(PropagatingMakeSpan::new())
                .on_request(|_: &Request<_>, span: &Span| {
                    let trace_id = TraceId::maybe_new()
                        .map(|t| t.to_string())
                        .unwrap_or_default();

                    span.record(TRACE_ID_FIELD_NAME, &trace_id.as_str());
                })
                .on_response(|resp: &Response<_>, duration: Duration, span: &Span| {
                    // Duration here is instant based
                    span.record("apollo_private.duration_ns", &(duration.as_nanos() as i64));
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

pub(super) fn main_router<RF>(configuration: &Configuration, apq: APQLayer) -> axum::Router
where
    RF: SupergraphServiceFactory,
{
    let mut graphql_configuration = configuration.supergraph.clone();
    if graphql_configuration.path.ends_with("/*") {
        // Needed for axum (check the axum docs for more information about wildcards https://docs.rs/axum/latest/axum/struct.Router.html#wildcards)
        graphql_configuration.path = format!("{}router_extra_path", graphql_configuration.path);
    }

    let apq2 = apq.clone();
    let get_handler = if configuration.sandbox.enabled {
        get({
            move |host: Host, Extension(service): Extension<RF>, http_request: Request<Body>| {
                handle_get_with_static(
                    Sandbox::display_page(),
                    host,
                    apq2,
                    service.new_service().boxed(),
                    http_request,
                )
            }
        })
    } else if configuration.homepage.enabled {
        get({
            move |host: Host, Extension(service): Extension<RF>, http_request: Request<Body>| {
                handle_get_with_static(
                    Homepage::display_page(),
                    host,
                    apq2,
                    service.new_service().boxed(),
                    http_request,
                )
            }
        })
    } else {
        get({
            move |host: Host, Extension(service): Extension<RF>, http_request: Request<Body>| {
                handle_get(host, apq2, service.new_service().boxed(), http_request)
            }
        })
    };

    Router::<hyper::Body>::new().route(
        &graphql_configuration.path,
        get_handler
            .post({
                move |host: Host,
                      uri: OriginalUri,
                      request: Json<graphql::Request>,
                      Extension(service): Extension<RF>,
                      header_map: HeaderMap| {
                    {
                        handle_post(
                            host,
                            uri,
                            request,
                            apq,
                            service.new_service().boxed(),
                            header_map,
                        )
                    }
                }
            })
            .layer(middleware::from_fn(check_accept_header)),
    )
}
