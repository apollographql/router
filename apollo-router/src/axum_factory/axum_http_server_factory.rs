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
use bytes::Bytes;
use futures::channel::oneshot;
use futures::future::join;
use futures::future::join_all;
use futures::prelude::*;
use http::Request;
use hyper::Body;
use itertools::Itertools;
use multimap::MultiMap;
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
use super::utils::decompress_request_body;
use super::utils::PropagatingMakeSpan;
use super::ListenAddrAndRouter;
use crate::axum_factory::listeners::get_extra_listeners;
use crate::axum_factory::listeners::serve_router_on_listen_addr;
use crate::configuration::Configuration;
use crate::configuration::Homepage;
use crate::configuration::ListenAddr;
use crate::configuration::Sandbox;
use crate::graphql;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::http_server_factory::Listener;
use crate::router::ApolloRouterError;
use crate::router_factory::Endpoint;
use crate::router_factory::SupergraphServiceFactory;
use crate::services::transport;

/// A basic http server using Axum.
/// Uses streaming as primary method of response.
#[derive(Debug)]
pub(crate) struct AxumHttpServerFactory;

impl AxumHttpServerFactory {
    pub(crate) fn new() -> Self {
        Self
    }
}

pub(crate) fn make_axum_router<RF>(
    service_factory: RF,
    configuration: &Configuration,
    mut endpoints: MultiMap<ListenAddr, Endpoint>,
) -> Result<ListenersAndRouters, ApolloRouterError>
where
    RF: SupergraphServiceFactory,
{
    ensure_listenaddrs_consistency(configuration, &endpoints)?;

    endpoints.insert(
        configuration.supergraph.listen.clone(),
        Endpoint::new(
            "/.well-known/apollo/server-health".to_string(),
            service_fn(|_req: transport::Request| async move {
                Ok::<_, BoxError>(
                    http::Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(
                            Bytes::from_static(b"The health check is no longer at this endpoint")
                                .into(),
                        )
                        .unwrap(),
                )
            })
            .boxed(),
        ),
    );

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
        RF: SupergraphServiceFactory,
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
) -> Result<ListenAddrAndRouter, ApolloRouterError>
where
    RF: SupergraphServiceFactory,
{
    let cors = configuration.cors.clone().into_layer().map_err(|e| {
        ApolloRouterError::ServiceCreationError(format!("CORS configuration error: {e}").into())
    })?;

    let main_route = main_router::<RF>(configuration)
        .layer(middleware::from_fn(decompress_request_body))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(PropagatingMakeSpan::new())
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

pub(super) fn main_router<RF>(configuration: &Configuration) -> axum::Router
where
    RF: SupergraphServiceFactory,
{
    let mut graphql_configuration = configuration.supergraph.clone();
    if graphql_configuration.path.ends_with("/*") {
        // Needed for axum (check the axum docs for more information about wildcards https://docs.rs/axum/latest/axum/struct.Router.html#wildcards)
        graphql_configuration.path = format!("{}router_extra_path", graphql_configuration.path);
    }

    let get_handler = if configuration.sandbox.enabled {
        get({
            move |host: Host, Extension(service): Extension<RF>, http_request: Request<Body>| {
                handle_get_with_static(
                    Sandbox::display_page(),
                    host,
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
                    service.new_service().boxed(),
                    http_request,
                )
            }
        })
    } else {
        get({
            move |host: Host, Extension(service): Extension<RF>, http_request: Request<Body>| {
                handle_get(host, service.new_service().boxed(), http_request)
            }
        })
    };

    Router::<hyper::Body>::new().route(
        &graphql_configuration.path,
        get_handler.post({
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
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use async_compression::tokio::write::GzipDecoder;
    use async_compression::tokio::write::GzipEncoder;
    use futures::Stream;
    use futures::StreamExt;
    use http::header::ACCEPT_ENCODING;
    use http::header::CONTENT_ENCODING;
    use http::header::CONTENT_TYPE;
    use http::header::{self};
    use http::HeaderValue;
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
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tower::service_fn;
    use tower::Service;

    use super::*;
    use crate::configuration::Cors;
    use crate::configuration::Homepage;
    use crate::configuration::Sandbox;
    use crate::configuration::Supergraph;
    use crate::http_ext;
    use crate::json_ext::Path;
    use crate::services::new_service::NewService;
    use crate::services::transport;
    use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
    use crate::test_harness::http_client;
    use crate::test_harness::http_client::MaybeMultipart;
    use crate::TestHarness;

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
        SupergraphService {
            fn service_call(&mut self, req: http::Request<graphql::Request>) -> Result<http::Response<graphql::ResponseStream>, BoxError>;
        }
    }

    type MockSupergraphServiceType = tower_test::mock::Mock<
        http::Request<graphql::Request>,
        http::Response<Pin<Box<dyn Stream<Item = graphql::Response> + Send>>>,
    >;

    #[derive(Clone)]
    struct TestSupergraphServiceFactory {
        inner: MockSupergraphServiceType,
    }

    impl NewService<http::Request<graphql::Request>> for TestSupergraphServiceFactory {
        type Service = MockSupergraphServiceType;

        fn new_service(&self) -> Self::Service {
            self.inner.clone()
        }
    }

    impl SupergraphServiceFactory for TestSupergraphServiceFactory {
        type SupergraphService = MockSupergraphServiceType;

        type Future = <<TestSupergraphServiceFactory as NewService<
            http::Request<graphql::Request>,
        >>::Service as Service<http::Request<graphql::Request>>>::Future;

        fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
            MultiMap::new()
        }
    }

    async fn init(mut mock: MockSupergraphService) -> (HttpServerHandle, Client) {
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
                TestSupergraphServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(
                    Configuration::fake_builder()
                        .sandbox(
                            crate::configuration::Sandbox::fake_builder()
                                .enabled(true)
                                .build(),
                        )
                        .supergraph(
                            crate::configuration::Supergraph::fake_builder()
                                .introspection(true)
                                .build(),
                        )
                        .homepage(
                            crate::configuration::Homepage::fake_builder()
                                .enabled(false)
                                .build(),
                        )
                        .build()
                        .unwrap(),
                ),
                None,
                vec![],
                MultiMap::new(),
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
        mut mock: MockSupergraphService,
        conf: Configuration,
        web_endpoints: MultiMap<ListenAddr, Endpoint>,
    ) -> Result<(HttpServerHandle, Client), ApolloRouterError> {
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
                TestSupergraphServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(conf),
                None,
                vec![],
                web_endpoints,
            )
            .await?;
        let mut default_headers = HeaderMap::new();
        default_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .redirect(Policy::none())
            .build()
            .unwrap();
        Ok((server, client))
    }

    #[cfg(unix)]
    async fn init_unix(
        mut mock: MockSupergraphService,
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
                TestSupergraphServiceFactory {
                    inner: service.into_inner(),
                },
                Arc::new(
                    Configuration::fake_builder()
                        .supergraph(
                            crate::configuration::Supergraph::fake_builder()
                                .listen(ListenAddr::UnixSocket(temp_dir.as_ref().join("sock")))
                                .build(),
                        )
                        .build()
                        .unwrap(),
                ),
                None,
                vec![],
                MultiMap::new(),
            )
            .await
            .expect("Failed to create server factory");

        server
    }

    #[tokio::test]
    async fn it_displays_sandbox() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();

        let conf = Configuration::fake_builder()
            .sandbox(Sandbox::fake_builder().enabled(true).build())
            .homepage(Homepage::fake_builder().enabled(false).build())
            .supergraph(Supergraph::fake_builder().introspection(true).build())
            .build()
            .unwrap();

        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;

        // Regular studio redirect
        let response = client
            .get(&format!(
                "{}/",
                server.graphql_listen_address().as_ref().unwrap()
            ))
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
        assert_eq!(response.bytes().await.unwrap(), Sandbox::display_page());

        Ok(())
    }

    #[tokio::test]
    async fn it_displays_sandbox_with_different_supergraph_path() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();

        let conf = Configuration::fake_builder()
            .sandbox(Sandbox::fake_builder().enabled(true).build())
            .homepage(Homepage::fake_builder().enabled(false).build())
            .supergraph(
                Supergraph::fake_builder()
                    .introspection(true)
                    .path("/custom")
                    .build(),
            )
            .build()
            .unwrap();

        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;

        // Regular studio redirect
        let response = client
            .get(&format!(
                "{}/custom",
                server.graphql_listen_address().as_ref().unwrap()
            ))
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
        assert_eq!(response.bytes().await.unwrap(), Sandbox::display_page());

        Ok(())
    }

    #[tokio::test]
    async fn it_compress_response_body() -> Result<(), ApolloRouterError> {
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yayyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"})) // Body must be bigger than 32 to be compressed
            .build();
        let example_response = expected_response.clone();
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_req| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

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
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(1)
            .withf(move |req| {
                assert_eq!(req.body().query.as_ref().unwrap(), "query");
                true
            })
            .returning(move |_req| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

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
        let expectations = MockSupergraphService::new();
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!(
                "{}/",
                server.graphql_listen_address().as_ref().unwrap()
            ))
            .body("Garbage")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.shutdown().await
    }

    #[tokio::test]
    async fn response() -> Result<(), ApolloRouterError> {
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

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
        Ok(())
    }

    #[tokio::test]
    async fn bad_response() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();
        let (server, client) = init(expectations).await;
        let url = format!("{}/test", server.graphql_listen_address().as_ref().unwrap());

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
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::fake_builder()
            .supergraph(
                crate::configuration::Supergraph::fake_builder()
                    .path(String::from("/graphql"))
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;
        let url = format!(
            "{}/graphql",
            server.graphql_listen_address().as_ref().unwrap()
        );

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
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::fake_builder()
            .supergraph(
                crate::configuration::Supergraph::fake_builder()
                    .path(String::from("/:my_prefix/graphql"))
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;
        let url = format!(
            "{}/prefix/graphql",
            server.graphql_listen_address().as_ref().unwrap()
        );

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
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(4)
            .returning(move |_| {
                let example_response = example_response.clone();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let conf = Configuration::fake_builder()
            .supergraph(
                crate::configuration::Supergraph::fake_builder()
                    .path(String::from("/graphql/*"))
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;
        for url in &[
            format!(
                "{}/graphql/test",
                server.graphql_listen_address().as_ref().unwrap()
            ),
            format!(
                "{}/graphql/anothertest",
                server.graphql_listen_address().as_ref().unwrap()
            ),
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

        let mut expectations = MockSupergraphService::new();
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
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

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

        let mut expectations = MockSupergraphService::new();
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
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

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
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |_| {
                let example_response = crate::error::FetchError::SubrequestHttpError {
                    service: "Mock service".to_string(),
                    reason: "Mock error".to_string(),
                }
                .to_response();
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;

        let response = client
            .post(format!(
                "{}/",
                server.graphql_listen_address().as_ref().unwrap()
            ))
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
        let expectations = MockSupergraphService::new();
        let conf = Configuration::fake_builder()
            .cors(Cors::builder().build())
            .supergraph(
                crate::configuration::Supergraph::fake_builder()
                    .path(String::from("/graphql/*"))
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;

        let response = client
            .request(
                Method::OPTIONS,
                &format!(
                    "{}/graphql/",
                    server.graphql_listen_address().as_ref().unwrap()
                ),
            )
            .header(ACCEPT, "text/html")
            .header(ORIGIN, "https://studio.apollographql.com")
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
            vec!["https://studio.apollographql.com"],
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

        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |_| {
                let example_response = example_response.clone();

                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(example_response)
                        .unwrap(),
                ))
            });
        let server = init_unix(expectations, &temp_dir).await;

        let output = send_to_unix_socket(
            server.graphql_listen_address().as_ref().unwrap(),
            Method::POST,
            r#"{"query":"query"}"#,
        )
        .await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        // Get query
        let output = send_to_unix_socket(
            server.graphql_listen_address().as_ref().unwrap(),
            Method::GET,
            r#"query=query"#,
        )
        .await;

        assert_eq!(
            serde_json::from_slice::<graphql::Response>(&output).unwrap(),
            expected_response,
        );

        server.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    async fn send_to_unix_socket(addr: &ListenAddr, method: Method, body: &str) -> Vec<u8> {
        use tokio::io::AsyncBufReadExt;
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
    async fn test_health_check_returns_four_oh_four() {
        let expectations = MockSupergraphService::new();
        let (server, client) = init(expectations).await;
        let url = format!(
            "{}/.well-known/apollo/server-health",
            server.graphql_listen_address().as_ref().unwrap()
        );

        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test(tokio::test)]
    async fn it_send_bad_content_type() -> Result<(), ApolloRouterError> {
        let query = "query";
        let operation_name = "operationName";

        let expectations = MockSupergraphService::new();
        let (server, client) = init(expectations).await;
        let url = format!("{}", server.graphql_listen_address().as_ref().unwrap());
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
    async fn it_doesnt_display_disabled_sandbox() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();
        let conf = Configuration::fake_builder()
            // sandbox is disabled by default, but homepage will take over if we dont disable it
            .homepage(
                crate::configuration::Homepage::fake_builder()
                    .enabled(false)
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;
        let response = client
            .get(&format!(
                "{}/",
                server.graphql_listen_address().as_ref().unwrap()
            ))
            .header(ACCEPT, "text/html")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_doesnt_display_disabled_homepage() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();
        let conf = Configuration::fake_builder()
            .homepage(
                crate::configuration::Homepage::fake_builder()
                    .enabled(false)
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) = init_with_config(expectations, conf, MultiMap::new()).await?;
        let response = client
            .get(&format!(
                "{}/",
                server.graphql_listen_address().as_ref().unwrap()
            ))
            .header(ACCEPT, "text/html")
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{:?}",
            response.text().await
        );

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_answers_to_custom_endpoint() -> Result<(), ApolloRouterError> {
        let expectations = MockSupergraphService::new();
        let endpoint = service_fn(|req: transport::Request| async move {
            Ok::<_, BoxError>(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .body(format!("{} + {}", req.method(), req.uri().path()).into())
                    .unwrap(),
            )
        })
        .boxed_clone();
        let mut web_endpoints = MultiMap::new();
        web_endpoints.insert(
            ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
            Endpoint::new("/a-custom-path".to_string(), endpoint.clone().boxed()),
        );
        web_endpoints.insert(
            ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
            Endpoint::new("/an-other-custom-path".to_string(), endpoint.boxed()),
        );

        let conf = Configuration::fake_builder().build().unwrap();
        let (server, client) = init_with_config(expectations, conf, web_endpoints).await?;

        for path in &["/a-custom-path", "/an-other-custom-path"] {
            let response = client
                .get(&format!(
                    "{}{}",
                    server.graphql_listen_address().as_ref().unwrap(),
                    path
                ))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.text().await.unwrap(), format!("GET + {}", path));
        }

        for path in &["/a-custom-path", "/an-other-custom-path"] {
            let response = client
                .post(&format!(
                    "{}{}",
                    server.graphql_listen_address().as_ref().unwrap(),
                    path
                ))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.text().await.unwrap(), format!("POST + {}", path));
        }
        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn it_refuses_to_start_if_homepage_and_sandbox_are_enabled() {
        let error = Configuration::fake_builder()
            .homepage(crate::configuration::Homepage::fake_builder().build())
            .sandbox(
                crate::configuration::Sandbox::fake_builder()
                    .enabled(true)
                    .build(),
            )
            .build()
            .unwrap_err();

        assert_eq!(
            "sandbox and homepage cannot be enabled at the same time: disable the homepage if you want to enable sandbox",
            error.to_string()
        )
    }

    #[test(tokio::test)]
    async fn it_refuses_to_start_if_sandbox_is_enabled_and_introspection_is_not() {
        let error = Configuration::fake_builder()
            .homepage(crate::configuration::Homepage::fake_builder().build())
            .sandbox(
                crate::configuration::Sandbox::fake_builder()
                    .enabled(true)
                    .build(),
            )
            .supergraph(
                crate::configuration::Supergraph::fake_builder()
                    .introspection(false)
                    .build(),
            )
            .build()
            .unwrap_err();

        assert_eq!(
            "sandbox and homepage cannot be enabled at the same time: disable the homepage if you want to enable sandbox",
            error.to_string()
        )
    }

    #[test(tokio::test)]
    async fn it_refuses_to_bind_two_extra_endpoints_on_the_same_path() {
        let endpoint = service_fn(|req: transport::Request| async move {
            Ok::<_, BoxError>(
                http::Response::builder()
                    .status(StatusCode::OK)
                    .body(format!("{} + {}", req.method(), req.uri().path()).into())
                    .unwrap(),
            )
        })
        .boxed_clone();

        let mut web_endpoints = MultiMap::new();
        web_endpoints.insert(
            ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
            Endpoint::new("/a-custom-path".to_string(), endpoint.clone().boxed()),
        );
        web_endpoints.insert(
            ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
            Endpoint::new("/a-custom-path".to_string(), endpoint.boxed()),
        );

        let conf = Configuration::fake_builder().build().unwrap();
        let error = init_with_config(MockSupergraphService::new(), conf, web_endpoints)
            .await
            .unwrap_err();

        assert_eq!(
            "tried to register two endpoints on `127.0.0.1:0/a-custom-path`",
            error.to_string()
        )
    }

    #[test(tokio::test)]
    async fn it_checks_the_shape_of_router_request() -> Result<(), ApolloRouterError> {
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(2)
            .returning(move |req| {
                Ok(http_ext::from_response_to_stream(
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
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());
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

    #[tokio::test]
    async fn cors_origin_default() -> Result<(), ApolloRouterError> {
        let (server, client) = init(MockSupergraphService::new()).await;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        let response =
            request_cors_with_origin(&client, url.as_str(), "https://studio.apollographql.com")
                .await;
        assert_cors_origin(response, "https://studio.apollographql.com");

        let response =
            request_cors_with_origin(&client, url.as_str(), "https://this.wont.work.com").await;
        assert_not_cors_origin(response, "https://this.wont.work.com");
        Ok(())
    }

    #[tokio::test]
    async fn cors_allow_any_origin() -> Result<(), ApolloRouterError> {
        let conf = Configuration::fake_builder()
            .cors(Cors::builder().allow_any_origin(true).build())
            .build()
            .unwrap();
        let (server, client) =
            init_with_config(MockSupergraphService::new(), conf, MultiMap::new()).await?;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        let response =
            request_cors_with_origin(&client, url.as_str(), "https://thisisatest.com").await;
        assert_cors_origin(response, "*");

        Ok(())
    }

    #[tokio::test]
    async fn cors_origin_list() -> Result<(), ApolloRouterError> {
        let valid_origin = "https://thisoriginisallowed.com";

        let conf = Configuration::fake_builder()
            .cors(
                Cors::builder()
                    .origins(vec![valid_origin.to_string()])
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) =
            init_with_config(MockSupergraphService::new(), conf, MultiMap::new()).await?;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        let response = request_cors_with_origin(&client, url.as_str(), valid_origin).await;
        assert_cors_origin(response, valid_origin);

        let response =
            request_cors_with_origin(&client, url.as_str(), "https://thisoriginisinvalid").await;
        assert_not_cors_origin(response, "https://thisoriginisinvalid");

        Ok(())
    }

    #[tokio::test]
    async fn cors_origin_regex() -> Result<(), ApolloRouterError> {
        let apollo_subdomains = "https://([a-z0-9]+[.])*apollographql[.]com";

        let conf = Configuration::fake_builder()
            .cors(
                Cors::builder()
                    .origins(vec!["https://anexactmatchorigin.com".to_string()])
                    .match_origins(vec![apollo_subdomains.to_string()])
                    .build(),
            )
            .build()
            .unwrap();
        let (server, client) =
            init_with_config(MockSupergraphService::new(), conf, MultiMap::new()).await?;
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

        // regex tests
        let response =
            request_cors_with_origin(&client, url.as_str(), "https://www.apollographql.com").await;
        assert_cors_origin(response, "https://www.apollographql.com");
        let response =
            request_cors_with_origin(&client, url.as_str(), "https://staging.apollographql.com")
                .await;
        assert_cors_origin(response, "https://staging.apollographql.com");
        let response =
            request_cors_with_origin(&client, url.as_str(), "https://thisshouldnotwork.com").await;
        assert_not_cors_origin(response, "https://thisshouldnotwork.com");

        // exact match tests
        let response =
            request_cors_with_origin(&client, url.as_str(), "https://anexactmatchorigin.com").await;
        assert_cors_origin(response, "https://anexactmatchorigin.com");

        // won't match
        let response =
            request_cors_with_origin(&client, url.as_str(), "https://thisshouldnotwork.com").await;
        assert_not_cors_origin(response, "https://thisshouldnotwork.com");

        Ok(())
    }

    async fn request_cors_with_origin(
        client: &Client,
        url: &str,
        origin: &str,
    ) -> reqwest::Response {
        client
            .request(Method::OPTIONS, url)
            .header("Origin", origin)
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "content-type")
            .send()
            .await
            .unwrap()
    }

    fn assert_cors_origin(response: reqwest::Response, origin: &str) {
        assert!(response.status().is_success());
        let headers = response.headers();
        assert_headers_valid(&response);
        assert!(origin_valid(headers, origin));
    }

    fn assert_not_cors_origin(response: reqwest::Response, origin: &str) {
        assert!(response.status().is_success());
        let headers = response.headers();
        assert!(!origin_valid(headers, origin));
    }

    fn assert_headers_valid(response: &reqwest::Response) {
        assert_header_contains!(response, ACCESS_CONTROL_ALLOW_METHODS, &["POST"]);
        assert_header_contains!(response, ACCESS_CONTROL_ALLOW_HEADERS, &["content-type"]);
    }

    fn origin_valid(headers: &HeaderMap, origin: &str) -> bool {
        headers
            .get("access-control-allow-origin")
            .map(|h| h.to_str().map(|o| o == origin).unwrap_or_default())
            .unwrap_or_default()
    }

    #[test(tokio::test)]
    async fn response_shape() -> Result<(), ApolloRouterError> {
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |_| {
                Ok(http_ext::from_response_to_stream(
                    http::Response::builder()
                        .status(200)
                        .body(
                            graphql::Response::builder()
                                .data(json!({
                                    "test": "hello"
                                }))
                                .build(),
                        )
                        .unwrap(),
                ))
            });
        let (server, client) = init(expectations).await;
        let query = json!(
        {
          "query": "query { test }",
        });
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());
        let response = client
            .post(&url)
            .body(query.to_string())
            .send()
            .await
            .unwrap();

        println!("response: {:?}", response);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );

        assert_eq!(
            response.text().await.unwrap(),
            serde_json::to_string(&json!({
                "data": {
                    "test": "hello"
                },
            }))
            .unwrap()
        );

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn deferred_response_shape() -> Result<(), ApolloRouterError> {
        let mut expectations = MockSupergraphService::new();
        expectations
            .expect_service_call()
            .times(1)
            .returning(move |_| {
                let body = stream::iter(vec![
                    graphql::Response::builder()
                        .data(json!({
                            "test": "hello",
                        }))
                        .has_next(true)
                        .build(),
                    graphql::Response::builder()
                        .incremental(vec![graphql::IncrementalResponse::builder()
                            .data(json!({
                                "other": "world"
                            }))
                            .path(Path::default())
                            .build()])
                        .has_next(true)
                        .build(),
                    graphql::Response::builder().has_next(false).build(),
                ])
                .boxed();
                Ok(http::Response::builder().status(200).body(body).unwrap())
            });
        let (server, client) = init(expectations).await;
        let query = json!(
        {
          "query": "query { test ... @defer { other } }",
        });
        let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());
        let mut response = client
            .post(&url)
            .body(query.to_string())
            .send()
            .await
            .unwrap();

        println!("response: {:?}", response);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE))
        );

        let first = response.chunk().await.unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&*first).unwrap(),
            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"data\":{\"test\":\"hello\"},\"hasNext\":true}\r\n--graphql\r\n"
        );

        let second = response.chunk().await.unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&*second).unwrap(),
        "content-type: application/json\r\n\r\n{\"hasNext\":true,\"incremental\":[{\"data\":{\"other\":\"world\"},\"path\":[]}]}\r\n--graphql\r\n"
        );

        let third = response.chunk().await.unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&*third).unwrap(),
            "content-type: application/json\r\n\r\n{\"hasNext\":false}\r\n--graphql--\r\n"
        );

        server.shutdown().await
    }

    #[tokio::test]
    async fn it_makes_sure_same_listenaddrs_are_accepted() {
        let configuration = Configuration::fake_builder().build().unwrap();

        init_with_config(MockSupergraphService::new(), configuration, MultiMap::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_makes_sure_different_listenaddrs_but_same_port_are_not_accepted() {
        let configuration = Configuration::fake_builder()
            .supergraph(
                Supergraph::fake_builder()
                    .listen(SocketAddr::from_str("127.0.0.1:4010").unwrap())
                    .build(),
            )
            .sandbox(Sandbox::fake_builder().build())
            .build()
            .unwrap();

        let endpoint = service_fn(|_req: transport::Request| async move {
            Ok::<_, BoxError>(
                http::Response::builder()
                    .body("this is a test".to_string().into())
                    .unwrap(),
            )
        })
        .boxed();

        let mut web_endpoints = MultiMap::new();
        web_endpoints.insert(
            SocketAddr::from_str("0.0.0.0:4010").unwrap().into(),
            Endpoint::new("/".to_string(), endpoint),
        );

        let error = init_with_config(MockSupergraphService::new(), configuration, web_endpoints)
            .await
            .unwrap_err();
        assert_eq!(
            "tried to bind 127.0.0.1 and 0.0.0.0 on port 4010",
            error.to_string()
        )
    }

    #[tokio::test]
    async fn it_makes_sure_extra_endpoints_cant_use_the_same_listenaddr_and_path() {
        let configuration = Configuration::fake_builder()
            .supergraph(
                Supergraph::fake_builder()
                    .listen(SocketAddr::from_str("127.0.0.1:4010").unwrap())
                    .build(),
            )
            .build()
            .unwrap();
        let endpoint = service_fn(|_req: transport::Request| async move {
            Ok::<_, BoxError>(
                http::Response::builder()
                    .body("this is a test".to_string().into())
                    .unwrap(),
            )
        })
        .boxed();

        let mut mm = MultiMap::new();
        mm.insert(
            SocketAddr::from_str("127.0.0.1:4010").unwrap().into(),
            Endpoint::new("/".to_string(), endpoint),
        );

        let error = init_with_config(MockSupergraphService::new(), configuration, mm)
            .await
            .unwrap_err();

        assert_eq!(
            "tried to register two endpoints on `127.0.0.1:4010/`",
            error.to_string()
        )
    }

    #[tokio::test]
    async fn it_supports_server_restart() {
        let configuration = Arc::new(
            Configuration::fake_builder()
                .supergraph(
                    Supergraph::fake_builder()
                        .listen(SocketAddr::from_str("127.0.0.1:4010").unwrap())
                        .build(),
                )
                .build()
                .unwrap(),
        );

        let server_factory = AxumHttpServerFactory::new();
        let (service, _) = tower_test::mock::spawn();

        let supergraph_service_factory = TestSupergraphServiceFactory {
            inner: service.into_inner(),
        };

        let server = server_factory
            .create(
                supergraph_service_factory.clone(),
                configuration,
                None,
                vec![],
                MultiMap::new(),
            )
            .await
            .expect("Failed to create server factory");

        assert_eq!(
            ListenAddr::SocketAddr(SocketAddr::from_str("127.0.0.1:4010").unwrap()),
            server.graphql_listen_address().clone().unwrap()
        );

        // change the listenaddr
        let new_configuration = Arc::new(
            Configuration::fake_builder()
                .supergraph(
                    Supergraph::fake_builder()
                        .listen(SocketAddr::from_str("127.0.0.1:4020").unwrap())
                        .build(),
                )
                .build()
                .unwrap(),
        );

        let new_server = server
            .restart(
                &server_factory,
                supergraph_service_factory,
                new_configuration,
                MultiMap::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            ListenAddr::SocketAddr(SocketAddr::from_str("127.0.0.1:4020").unwrap()),
            new_server.graphql_listen_address().clone().unwrap()
        );
    }

    /// A counter of how many GraphQL responses have been sent by an Apollo Router
    ///
    /// When `@defer` is used, it should increment multiple times for a single HTTP request.
    #[derive(Clone, Default)]
    struct GraphQLResponseCounter(Arc<AtomicU32>);

    impl GraphQLResponseCounter {
        fn increment(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn get(&self) -> u32 {
            self.0.load(Ordering::SeqCst)
        }
    }

    async fn http_service() -> impl Service<
        http::Request<serde_json::Value>,
        Response = http::Response<MaybeMultipart<serde_json::Value>>,
        Error = BoxError,
    > {
        let counter = GraphQLResponseCounter::default();
        let service = TestHarness::builder()
            .configuration_json(json!({
                "plugins": {
                    "apollo.include_subgraph_errors": {
                        "all": true
                    }
                }
            }))
            .unwrap()
            .supergraph_hook(move |service| {
                let counter = counter.clone();
                service
                    .map_response(move |mut response| {
                        response.response.extensions_mut().insert(counter.clone());
                        response.map_stream(move |graphql_response| {
                            counter.increment();
                            graphql_response
                        })
                    })
                    .boxed()
            })
            .build_http_service()
            .await
            .unwrap()
            .map_err(Into::into);
        let service = http_client::response_decompression(service);
        let service = http_client::defer_spec_20220824_multipart(service);
        http_client::json(service)
    }

    /// Creates an Apollo Router as an HTTP-level Tower service and makes one request.
    async fn make_request(
        request_body: serde_json::Value,
    ) -> http::Response<MaybeMultipart<serde_json::Value>> {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .header("host", "127.0.0.1")
            .body(request_body)
            .unwrap();
        http_service().await.oneshot(request).await.unwrap()
    }

    fn assert_compressed<B>(response: &http::Response<B>, expected: bool) {
        assert_eq!(
            response
                .extensions()
                .get::<http_client::ResponseBodyWasCompressed>()
                .unwrap()
                .0,
            expected
        )
    }

    #[tokio::test]
    async fn test_compressed_response() {
        let response = make_request(json!({
            "query": "
                query TopProducts($first: Int) { 
                    topProducts(first: $first) { 
                        upc 
                        name 
                        reviews { 
                            id 
                            product { name } 
                            author { id name } 
                        } 
                    } 
                }
            ",
            "variables": {"first": 2_u32},
        }))
        .await;
        assert_compressed(&response, true);
        let status = response.status().as_u16();
        let graphql_response = response.into_body().expect_not_multipart();
        assert_eq!(graphql_response["errors"], json!(null));
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn test_defer_is_not_buffered() {
        let mut response = make_request(json!({
            "query": "
                query TopProducts($first: Int) { 
                    topProducts(first: $first) { 
                        upc 
                        name 
                        reviews { 
                            id 
                            product { name } 
                            ... @defer { author { id name } }
                        } 
                    } 
                }
            ",
            "variables": {"first": 2_u32},
        }))
        .await;
        assert_compressed(&response, false);
        let status = response.status().as_u16();
        assert_eq!(status, 200);
        let counter: GraphQLResponseCounter = response.extensions_mut().remove().unwrap();
        let parts = response.into_body().expect_multipart();

        let (parts, counts): (Vec<_>, Vec<_>) =
            parts.map(|part| (part, counter.get())).unzip().await;
        let parts = serde_json::Value::Array(parts);
        assert_eq!(
            parts,
            json!([
                {
                    "data": {
                        "topProducts": [
                            {"upc": "1", "name": "Table", "reviews": null},
                            {"upc": "2", "name": "Couch", "reviews": null}
                        ]
                    },
                    "errors": [
                        {
                            "message": "invalid content: Missing key `_entities`!",
                            "path": ["topProducts", "@"],
                            "extensions": {
                                "type": "ExecutionInvalidContent",
                                "reason": "Missing key `_entities`!"
                            }
                        }],
                    "hasNext": true,
                },
                {"hasNext": false}
            ]),
            "{}",
            serde_json::to_string(&parts).unwrap()
        );

        // Non-regression test for https://github.com/apollographql/router/issues/1572
        //
        // With unpatched async-compression 0.3.14 as used by tower-http 0.3.4,
        // `counts` is `[2, 2]` since both parts have to be generated on the server side
        // before the first one reaches the client.
        //
        // Conversly, observing the value `1` after receiving the first part
        // means the didn’t wait for all parts to be in the compression buffer
        // before sending any.
        assert_eq!(counts, [1, 2]);
    }
}
