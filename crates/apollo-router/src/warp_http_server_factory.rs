use crate::configuration::{Configuration, Cors};
use crate::http_server_factory::{HttpServerFactory, HttpServerHandle};
use crate::FederatedServerError;
use apollo_router_core::prelude::*;
use bytes::Bytes;
use futures::channel::oneshot;
use futures::prelude::*;
use std::pin::Pin;
use std::sync::Arc;
use tracing::Instrument;

use warp::host::Authority;
use warp::hyper::Response;
use warp::{
    http::{StatusCode, Uri},
    hyper::Body,
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
    fn create<F>(
        &self,
        graph: Arc<F>,
        configuration: Arc<Configuration>,
    ) -> Pin<Box<dyn Future<Output = HttpServerHandle> + Send>>
    where
        F: graphql::Fetcher + 'static,
    {
        let f = async {
            let (shutdown_sender, shutdown_receiver) = oneshot::channel();
            let listen_address = configuration.server.listen;

            let cors = configuration
                .server
                .cors
                .as_ref()
                .map(|cors_configuration| cors_configuration.into_warp_middleware())
                .unwrap_or_else(|| Cors::builder().build().into_warp_middleware());

            let routes = run_get_query_or_redirect(Arc::clone(&graph), Arc::clone(&configuration))
                .await
                .or(perform_graphql_request(graph, configuration).await)
                .with(cors);

            let (actual_listen_address, server) =
                warp::serve(routes).bind_with_graceful_shutdown(listen_address, async {
                    shutdown_receiver.await.ok();
                });

            // Spawn the server into a runtime
            let server_future = tokio::task::spawn(server)
                .map_err(|_| FederatedServerError::HttpServerLifecycleError)
                .boxed();

            HttpServerHandle {
                shutdown_sender,
                server_future,
                listen_address: actual_listen_address,
            }
        };

        Box::pin(f)
    }
}

async fn run_get_query_or_redirect<F>(
    graph: Arc<F>,
    configuration: Arc<Configuration>,
) -> impl Filter<Extract = (Box<dyn Reply>,), Error = Rejection> + Clone
where
    F: graphql::Fetcher + 'static,
{
    let tracing_subscriber = configuration.subscriber.clone();

    warp::get()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::header::optional::<String>("accept"))
        .and(warp::host::optional())
        .and(warp::body::bytes())
        .and_then(
            move |accept: Option<String>, host: Option<Authority>, body: Bytes| {
                let graph = Arc::clone(&graph);
                let tracing_subscriber = tracing_subscriber.clone();
                let dispatcher = tracing_subscriber
                    .clone()
                    .map(tracing::Dispatch::new)
                    .unwrap_or_default();
                let span = tracing::info_span!("federated_query");

                async move {
                    let reply: Box<dyn Reply> = if accept.map(prefers_html).unwrap_or_default() {
                        redirect_to_studio(host)
                    } else if let Ok(request) = serde_json::from_slice(&body) {
                        // Run GraphQL request
                        let response_stream =
                            tracing::dispatcher::with_default(&dispatcher, || {
                                run_request(graph, request).instrument(span)
                            })
                            .await;

                        Box::new(Response::new(Body::wrap_stream(response_stream)))
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

async fn run_request<F>(
    graph: Arc<F>,
    request: graphql::Request,
) -> impl Stream<Item = Result<Bytes, serde_json::Error>>
where
    F: graphql::Fetcher + 'static,
{
    let stream = graph.stream(request);

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

async fn perform_graphql_request<F>(
    graph: Arc<F>,
    configuration: Arc<Configuration>,
) -> impl Filter<Extract = (Response<Body>,), Error = Rejection> + Clone
where
    F: graphql::Fetcher + 'static,
{
    let tracing_subscriber = configuration.subscriber.clone();
    warp::post()
        .and(warp::path::end().or(warp::path("graphql")).unify())
        .and(warp::body::json())
        .and_then(move |request: graphql::Request| {
            let graph = Arc::clone(&graph);
            let tracing_subscriber = tracing_subscriber.clone();
            let dispatcher = tracing_subscriber
                .clone()
                .map(tracing::Dispatch::new)
                .unwrap_or_default();
            let span = tracing::info_span!("federated_query");
            tracing::dispatcher::with_default(&dispatcher, || async move {
                Ok::<_, warp::reject::Rejection>(Response::new(Body::wrap_stream(
                    run_request(graph, request).instrument(span).await,
                )))
            })
        })
}

fn prefers_html(accept_header: String) -> bool {
    accept_header
        .split(',')
        .map(|a| a.trim())
        .find(|a| ["text/html", "application/json"].contains(a))
        == Some("text/html")
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

        impl graphql::Fetcher for MyFetcher {
            fn stream(&self, request: graphql::Request) -> graphql::ResponseStream;
        }
    }

    macro_rules! init {
        ($listen_address:expr, $fetcher:ident => $expect_stream:block) => {{
            let _ = env_logger::builder().is_test(true).try_init();
            #[allow(unused_mut)]
            let mut $fetcher = MockMyFetcher::new();
            $expect_stream;
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
                )
                .await;
            let client = reqwest::Client::builder()
                .redirect(Policy::none())
                .build()
                .unwrap();
            (server, client)
        }};
    }

    #[tokio::test]
    async fn redirect_to_studio() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        for url in vec![
            format!("http://{}/", server.listen_address),
            format!("http://{}/graphql", server.listen_address),
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
                    server.listen_address
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

    #[tokio::test]
    async fn malformed_request() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        let response = client
            .post(format!("http://{}/graphql", server.listen_address))
            .body("Garbage")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.shutdown().await
    }

    #[tokio::test]
    async fn response() -> Result<(), FederatedServerError> {
        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();
        let example_response = expected_response.clone();
        let (server, client) = init!("127.0.0.1:0", fetcher => {
            fetcher
                .expect_stream()
                .times(2)
                .returning(move |_| {
                    let actual_response = example_response.clone();
                    futures::stream::iter(vec![actual_response]).boxed()
                })
        });
        let url = format!("http://{}/graphql", server.listen_address);
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

    #[tokio::test]
    async fn response_failure() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {
            fetcher
                .expect_stream()
                .times(1)
                .return_once(|_| {
                    futures::stream::iter(vec![graphql::FetchError::SubrequestHttpError {
                        service: "Mock service".to_string(),
                        reason: "Mock error".to_string(),
                    }
                    .to_response(true)])
                    .boxed()
                })
        });
        let response = client
            .post(format!("http://{}/graphql", server.listen_address))
            .body(
                json!(
                {
                  "query": "query",
                })
                .to_string(),
            )
            .send()
            .await
            .ok()
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

    #[tokio::test]
    async fn cors_preflight() -> Result<(), FederatedServerError> {
        let (server, client) = init!("127.0.0.1:0", fetcher => {});

        for url in vec![
            format!("http://{}/", server.listen_address),
            format!("http://{}/graphql", server.listen_address),
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
}
