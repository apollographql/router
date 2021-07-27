use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use futures::channel::oneshot;
use futures::future::FutureExt;
use futures::StreamExt;
use futures::TryStreamExt;
use hyper::body::Bytes;
use hyper::header::{HOST, LOCATION};
use hyper::server::Server;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, StatusCode};

use configuration::Configuration;
use execution::{FetchError, GraphQLFetcher};

use crate::http_server_factory::{HttpServerFactory, HttpServerHandle};
use crate::FederatedServerError;
use log::error;

/// A basic http server using hyper.
/// Uses streaming as primary method of response.
/// Redirects to studio for GET requests.
#[derive(Debug)]
pub(crate) struct HyperHttpServerFactory;

impl HyperHttpServerFactory {
    pub(crate) fn new() -> Self {
        HyperHttpServerFactory
    }
}

impl HttpServerFactory for HyperHttpServerFactory {
    fn create<F>(
        &self,
        graph: Arc<RwLock<F>>,
        configuration: Arc<RwLock<Configuration>>,
    ) -> HttpServerHandle
    where
        F: GraphQLFetcher + 'static,
    {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listen_address = configuration.read().unwrap().listen; //unwrap-lock

        let server =
            Server::bind(&listen_address).serve(make_service_fn(move |_conn| {
                let graph = graph.to_owned();
                async move {
                    Ok::<_, Infallible>(service_fn(move |req| serve_req(req, graph.to_owned())))
                }
            }));
        let listen_address = server.local_addr().to_owned();
        let server_future = tokio::spawn(server.with_graceful_shutdown(async {
            shutdown_receiver.await.ok();
        }))
        .map(|result| match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_err)) => Err(FederatedServerError::HttpServerLifecycleError),
            Err(_err) => Err(FederatedServerError::HttpServerLifecycleError),
        })
        .boxed();

        HttpServerHandle {
            shutdown_sender,
            server_future,
            listen_address,
        }
    }
}

async fn serve_req<F>(
    request: Request<Body>,
    graph: Arc<RwLock<F>>,
) -> Result<Response<Body>, hyper::Error>
where
    F: GraphQLFetcher,
{
    let mut response = Response::new(Body::empty());

    match (request.method(), request.uri().path()) {
        (&Method::POST, "/graphql") => handle_graphql_request(request, graph, &mut response).await,
        (&Method::GET, "/") | (&Method::GET, "/graphql") => {
            handle_redirect_to_studio(request, &mut response)
        }

        _ => {
            *response.status_mut() = StatusCode::NOT_FOUND;
            *response.body_mut() = Body::from("");
        }
    };

    Ok(response)
}

async fn handle_graphql_request<F>(
    request: Request<Body>,
    graph: Arc<RwLock<F>>,
    response: &mut Response<Body>,
) where
    F: GraphQLFetcher,
{
    let (_header, body) = request.into_parts();
    // TODO Hardening. to_bytes does not reject huge requests.
    match hyper::body::to_bytes(body).await {
        Ok(bytes) => {
            let graphql_request = serde_json::from_slice(&bytes);
            match graphql_request {
                Ok(graphql_request) => {
                    *response.body_mut() = Body::wrap_stream(
                        graph
                            .read()
                            .unwrap()
                            .stream(graphql_request)
                            .map_ok(|chunk| match serde_json::to_string(&chunk) {
                                Ok(bytes) => Ok(Bytes::from(bytes)),
                                Err(_err) => Err(FetchError::MalformedResponseError),
                            })
                            .map(|res| match res {
                                Ok(Ok(ok)) => Ok(ok),
                                Ok(Err(err)) | Err(err) => Err(err),
                            })
                            .boxed(),
                    )
                }
                Err(err) => {
                    *response.status_mut() = StatusCode::BAD_REQUEST;
                    *response.body_mut() = Body::from(format!("Request was malformed: {}", err));
                }
            }
        }
        Err(err) => {
            error!("Could not read request: {}", err);
            *response.status_mut() = StatusCode::BAD_REQUEST;
            *response.body_mut() = Body::from("Could not read request.");
        }
    }
}

fn handle_redirect_to_studio(request: Request<Body>, response: &mut Response<Body>) {
    *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;

    response.headers_mut().insert(
        LOCATION,
        format!(
            "https://studio.apollographql.com/sandbox?endpoint=http://{}",
            request.headers().get(HOST).unwrap().to_str().unwrap()
        )
        .parse()
        .unwrap(),
    );
    *response.body_mut() = Body::from("");
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use futures::StreamExt;
    #[cfg(test)]
    use mockall::{mock, predicate::*};
    use reqwest::redirect::Policy;
    use reqwest::Client;
    use serde_json::json;

    use execution::{
        FetchError, GraphQLFetcher, GraphQLPrimaryResponse, GraphQLRequest, GraphQLResponse,
        GraphQLResponseStream,
    };

    use super::*;

    mock! {
        #[derive(Debug)]
        MyGraphQLFetcher{}
        impl GraphQLFetcher for MyGraphQLFetcher {   // specification of the trait to mock
            fn stream(&self, request: GraphQLRequest) -> GraphQLResponseStream;
        }
    }

    fn init(listen_address: &str) -> (Arc<RwLock<MockMyGraphQLFetcher>>, HttpServerHandle, Client) {
        let _ = env_logger::builder().is_test(true).try_init();
        let fetcher = MockMyGraphQLFetcher::new();
        let server_factory = HyperHttpServerFactory::new();
        let fetcher = Arc::new(RwLock::new(fetcher));
        let server = server_factory.create(
            fetcher.to_owned(),
            Arc::new(RwLock::new(
                Configuration::builder()
                    .listen(SocketAddr::from_str(listen_address).unwrap())
                    .subgraphs(HashMap::new())
                    .build(),
            )),
        );
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap();
        (fetcher, server, client)
    }

    #[tokio::test]
    async fn redirect_to_studio() -> Result<(), FederatedServerError> {
        // Use IPv6 just for fun.
        let (_fetcher, server, client) = init("[::1]:0");

        for url in vec![
            format!("http://{}/", server.listen_address),
            format!("http://{}/graphql", server.listen_address),
        ] {
            let response = client.get(url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(
                response.headers().get(LOCATION).unwrap().to_str().unwrap(),
                format!(
                    "https://studio.apollographql.com/sandbox?endpoint=http://{}",
                    server.listen_address
                )
                .to_string()
            );
        }

        server.shutdown().await
    }

    #[tokio::test]
    async fn malformed_request() -> Result<(), FederatedServerError> {
        let (_fetcher, server, client) = init("127.0.0.1:0");

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
        let expected_response = GraphQLPrimaryResponse {
            data: json!(
            {
              "response": "yay",
            })
            .as_object()
            .cloned()
            .unwrap(),
            has_next: None,
            errors: None,
            extensions: None,
        };
        let example_response = expected_response.clone();
        let (fetcher, server, client) = init("127.0.0.1:0");
        {
            fetcher
                .write()
                .unwrap()
                .expect_stream()
                .times(1)
                .return_once(move |_| {
                    futures::stream::iter(vec![Ok(GraphQLResponse::Primary(example_response))])
                        .boxed()
                });
        }
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
            .unwrap();

        assert_eq!(
            response.json::<GraphQLPrimaryResponse>().await.unwrap(),
            expected_response
        );
        server.shutdown().await
    }

    #[tokio::test]
    async fn response_failure() -> Result<(), FederatedServerError> {
        let (fetcher, server, client) = init("127.0.0.1:0");
        {
            fetcher
                .write()
                .unwrap()
                .expect_stream()
                .times(1)
                .return_once(|_| {
                    futures::stream::iter(vec![Err(FetchError::ServiceError {
                        service: "Mock service".to_string(),
                        reason: "Mock error".to_string(),
                    })])
                    .boxed()
                });
        }
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
            .err()
            .unwrap();

        // Why are we even testing this?
        // Basically for chunked encoding the only option to send errors back to the client are
        // via a trailer header, but even then, we can't send back an alternate error code.
        // Our only real option is to make sure that this code path doesn't happen and use graphql errors.
        // However, we don't want to bring down the server, so we don't panic.
        assert_eq!(
            format!("{:?}", response.source().unwrap()),
            "hyper::Error(IncompleteMessage)"
        );
        server.shutdown().await
    }
}
