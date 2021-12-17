use crate::configuration::{Configuration, Cors};
use crate::http_server_factory::{HttpServerFactory, HttpServerHandle};
use crate::FederatedServerError;
use apollo_router_core::prelude::*;
use bytes::Bytes;
use futures::{channel::oneshot, prelude::*};
use hyper::server::conn::Http;
use once_cell::sync::Lazy;
use opentelemetry::propagation::Extractor;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tracing::instrument::WithSubscriber;
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use warp::host::Authority;
use warp::{
    http::{header::HeaderMap, StatusCode, Uri},
    hyper::{Body, Response},
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
    fn create<Router, PreparedQuery>(
        &self,
        router: Arc<Router>,
        configuration: Arc<Configuration>,
        listener: Option<TcpListener>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>
    where
        Router: graphql::Router<PreparedQuery> + 'static,
        PreparedQuery: graphql::PreparedQuery + 'static,
    {
        Box::pin(async move {
            let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
            let listen_address = configuration.server.listen;

            let cors = configuration
                .server
                .cors
                .as_ref()
                .map(|cors_configuration| cors_configuration.into_warp_middleware())
                .unwrap_or_else(|| Cors::builder().build().into_warp_middleware());

            let dispatcher = configuration
                .subscriber
                .clone()
                .map(tracing::Dispatch::new)
                .unwrap_or_default();

            let routes = get_health_request()
                .or(get_graphql_request_or_redirect(Arc::clone(&router)))
                .or(post_graphql_request(router))
                .with(cors);

            // generate a hyper service from warp routes
            let svc = warp::service(routes);

            // if we received a TCP listener, reuse it, otherwise create a new one
            let tcp_listener = if let Some(listener) = listener {
                listener
            } else {
                TcpListener::bind(listen_address)
                    .await
                    .map_err(FederatedServerError::ServerCreationError)?
            };
            let actual_listen_address = tcp_listener
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

                loop {
                    tokio::select! {
                        _ = &mut shutdown_receiver => {
                            break;
                        }
                        res = tcp_listener.accept() => {
                            let svc = svc.clone();
                            let connection_shutdown = connection_shutdown.clone();

                            tokio::task::spawn(async move {
                                // we unwrap the result of accept() here to avoid stopping
                                // the entire server on an issue with that socket
                                // Unfortunately, the error here could also be linked
                                // to the listen socket (no RAM for kernel buffers, no
                                // more file descriptors, network interface is down...)
                                // ideally we'd want to handle the errors in the server task
                                // with varying behaviours
                                let (tcp_stream, _) = res.unwrap();
                                tcp_stream.set_nodelay(true).expect("this should not fail unless the socket is invalid");

                                let connection = Http::new()
                                    .http1_keep_alive(true)
                                    .serve_connection(tcp_stream, svc);

                                tokio::pin!(connection);
                                tokio::select! {
                                    // the connection finished first
                                    _res = &mut connection => {
                                        /*if let Err(http_err) = res {
                                            tracing::error!(
                                                "Error while serving HTTP connection: {}",
                                                http_err
                                            );
                                        }*/
                                    }
                                    // the shutdown receiver was triggered first,
                                    // so we tell the connection to do a graceful shutdown
                                    // on the next request, then we wait for it to finish
                                    _ = connection_shutdown.notified() => {
                                        let c = connection.as_mut();
                                        c.graceful_shutdown();

                                        if let Err(_http_err) = connection.await {
                                            /*tracing::error!(
                                                "Error while serving HTTP connection: {}",
                                                http_err
                                            );*/
                                        }
                                    }
                                }
                            }.with_subscriber(dispatcher.clone()));
                        }
                    }
                }

                // the shutdown receiver was triggered so we break out of
                // the server loop, tell the currently active connections to stop
                // then return the TCP listen socket
                connection_shutdown.notify_waiters();
                tcp_listener
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

fn get_graphql_request_or_redirect<Router, PreparedQuery>(
    router: Arc<Router>,
) -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone
where
    Router: graphql::Router<PreparedQuery> + 'static,
    PreparedQuery: graphql::PreparedQuery + 'static,
{
    warp::get()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::header::optional::<String>("accept"))
        .and(warp::host::optional())
        .and(warp::body::bytes())
        .and(warp::header::headers_cloned())
        .and_then(
            move |accept: Option<String>,
                  host: Option<Authority>,
                  body: Bytes,
                  header_map: HeaderMap| {
                let router = Arc::clone(&router);
                async move {
                    let reply: Box<dyn Reply> = if accept.map(prefers_html).unwrap_or_default() {
                        redirect_to_studio(host)
                    } else if let Ok(request) = serde_json::from_slice(&body) {
                        run_graphql_request(router, request, header_map).await
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
        .boxed()
}

fn redirect_to_studio(host: Option<Authority>) -> Box<dyn Reply> {
    // Try to redirect to Studio
    if host.is_some() {
        if let Ok(uri) = format!(
            "https://studio.apollographql.com/sandbox?endpoint=http://{}",
            // we made sure host.is_some() above
            host.unwrap()
        )
        .parse::<Uri>()
        {
            Box::new(warp::redirect::temporary(uri))
        } else {
            Box::new(warp::reply::with_status(
                "Invalid host to redirect to",
                StatusCode::BAD_REQUEST,
            ))
        }
    } else {
        Box::new(warp::reply::with_status(
            "Invalid host to redirect to",
            StatusCode::BAD_REQUEST,
        ))
    }
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

fn post_graphql_request<Router, PreparedQuery>(
    router: Arc<Router>,
) -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone
where
    Router: graphql::Router<PreparedQuery> + 'static,
    PreparedQuery: graphql::PreparedQuery + 'static,
{
    warp::post()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::body::json())
        .and(warp::header::headers_cloned())
        .and_then(move |request: graphql::Request, header_map: HeaderMap| {
            let router = Arc::clone(&router);
            async move {
                let reply = run_graphql_request(router, request, header_map).await;
                Ok::<_, warp::reject::Rejection>(reply)
            }
            .boxed()
        })
}

fn run_graphql_request<Router, PreparedQuery>(
    router: Arc<Router>,
    request: graphql::Request,
    header_map: HeaderMap,
) -> impl Future<Output = Box<dyn Reply>>
where
    Router: graphql::Router<PreparedQuery> + 'static,
    PreparedQuery: graphql::PreparedQuery + 'static,
{
    // retrieve and reuse the potential trace id from the caller
    opentelemetry::global::get_text_map_propagator(|injector| {
        injector.extract_with_context(&Span::current().context(), &HeaderMapCarrier(&header_map));
    });

    async move {
        let response_stream = stream_request(router, request)
            .instrument(tracing::info_span!("graphql_request"))
            .await;

        Box::new(Response::new(Body::wrap_stream(response_stream))) as Box<dyn Reply>
    }
}

async fn stream_request<Router, PreparedQuery>(
    router: Arc<Router>,
    request: graphql::Request,
) -> impl Stream<Item = Result<Bytes, serde_json::Error>>
where
    Router: graphql::Router<PreparedQuery> + 'static,
    PreparedQuery: graphql::PreparedQuery,
{
    let stream = match router.prepare_query(&request).await {
        Ok(route) => route.execute(Arc::new(request)).await,
        Err(stream) => stream,
    };

    stream
        .enumerate()
        .map(|(index, res)| match serde_json::to_string(&res) {
            Ok(bytes) => Ok(Bytes::from(bytes)),
            Err(err) => {
                // We didn't manage to serialise the response!
                // Do our best to send some sort of error back.
                serde_json::to_string(
                    &graphql::FetchError::MalformedResponse {
                        reason: err.to_string(),
                    }
                    .to_response(index == 0),
                )
                .map(Bytes::from)
            }
        })
}

fn prefers_html(accept_header: String) -> bool {
    accept_header
        .split(',')
        .map(|a| a.trim())
        .find(|a| ["text/html", "application/json"].contains(a))
        == Some("text/html")
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
    use mockall::{mock, predicate::*};
    use reqwest::header::{
        ACCEPT, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
        ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD,
        LOCATION, ORIGIN,
    };
    use reqwest::redirect::Policy;
    use reqwest::{Method, StatusCode};
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
        MyFetcher {}

        #[async_trait::async_trait]
        impl graphql::Fetcher for MyFetcher {
            async fn stream(&self, request: graphql::Request) -> graphql::ResponseStream;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRouter {}

        #[async_trait::async_trait]
        impl graphql::Router<MockMyRoute> for MyRouter {
            async fn prepare_query(
                &self,
                request: &graphql::Request,
            ) -> Result<MockMyRoute, graphql::ResponseStream>;
        }
    }

    mock! {
        #[derive(Debug)]
        MyRoute {}

        #[async_trait::async_trait]
        impl graphql::PreparedQuery for MyRoute {
            async fn execute(self, request: Arc<graphql::Request>) -> graphql::ResponseStream;
        }
    }

    macro_rules! init {
        ($listen_address:expr, $fetcher:ident => $expect_prepare_query:block) => {{
            #[allow(unused_mut)]
            let mut $fetcher = MockMyRouter::new();
            $expect_prepare_query;
            let server_factory = WarpHttpServerFactory::new();
            let fetcher = Arc::new($fetcher);
            let server = server_factory
                .create(
                    fetcher.to_owned(),
                    Arc::new(
                        Configuration::builder()
                            .server(
                                crate::configuration::Server::builder()
                                    .listen(SocketAddr::from_str($listen_address).unwrap())
                                    .cors(Some(
                                        Cors::builder()
                                            .origins(vec!["http://studio".to_string()])
                                            .build(),
                                    ))
                                    .build(),
                            )
                            .subgraphs(Default::default())
                            .build(),
                    ),
                    None,
                )
                .await?;
            let client = reqwest::Client::builder()
                .redirect(Policy::none())
                .build()
                .unwrap();
            (server, client)
        }};
    }

    #[test(tokio::test)]
    async fn redirect_to_studio() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        for url in vec![
            format!("http://{}/", server.listen_address()),
            format!("http://{}/graphql", server.listen_address()),
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
                StatusCode::TEMPORARY_REDIRECT,
                "{}",
                response.text().await.unwrap()
            );
            assert_header!(
                &response,
                LOCATION,
                vec![format!(
                    "https://studio.apollographql.com/sandbox?endpoint=http://{}",
                    server.listen_address()
                )],
                "Incorrect redirect url"
            );

            // application/json, but the query body is empty
            let response = client
                .get(url.as_str())
                .header(ACCEPT, "application/json")
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{}",
                response.text().await.unwrap(),
            );
        }

        server.shutdown().await
    }

    #[test(tokio::test)]
    async fn malformed_request() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        let response = client
            .post(format!("http://{}/graphql", server.listen_address()))
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
        let (server, client) = init!("127.0.0.1:0", fetcher => {
            fetcher
                .expect_prepare_query()
                .times(2)
                .returning(move |_| {
                    let example_response = example_response.clone();
                    let mut route = MockMyRoute::new();
                    route.expect_execute()
                        .times(1)
                        .return_once(move |_| {
                            example_response.into()
                        });
                    Ok(route)
                })
        });
        let url = format!("http://{}/graphql", server.listen_address());
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
        let (server, client) = init!("127.0.0.1:0", fetcher => {
            fetcher
                .expect_prepare_query()
                .times(1)
                .return_once(|_| {
                    let mut route = MockMyRoute::new();
                    route.expect_execute()
                        .times(1)
                        .return_once(|_| {
                            graphql::FetchError::SubrequestHttpError {
                                service: "Mock service".to_string(),
                                reason: "Mock error".to_string(),
                            }
                            .to_response(true).into()
                        });
                    Ok(route)
                })
        });
        let response = client
            .post(format!("http://{}/graphql", server.listen_address()))
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
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        for url in vec![
            format!("http://{}/", server.listen_address()),
            format!("http://{}/graphql", server.listen_address()),
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

    #[test]
    fn test_prefers_html() {
        use super::prefers_html;
        ["text/html,application/json", " text/html,application/json"]
            .iter()
            .for_each(|accepts| assert!(prefers_html(accepts.to_string())));

        ["application/json", "application/json,text/html"]
            .iter()
            .for_each(|accepts| assert!(!prefers_html(accepts.to_string())));
    }

    #[test(tokio::test)]
    async fn test_health_check() {
        let filter = get_health_request();

        let res = warp::test::request()
            .path("/.well-known/apollo/server-health")
            .reply(&filter)
            .await;

        insta::assert_debug_snapshot!(res);
    }
}
