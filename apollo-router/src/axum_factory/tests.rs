use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::GzipEncoder;
use axum::body::BoxBody;
use futures::future::BoxFuture;
use futures::stream;
use futures::stream::poll_fn;
use futures::Future;
use futures::StreamExt;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::{self};
use http::HeaderMap;
use http::HeaderValue;
use http_body::Body;
use mime::APPLICATION_JSON;
use mockall::mock;
use multimap::MultiMap;
use reqwest::header::ACCEPT;
use reqwest::header::ACCESS_CONTROL_ALLOW_HEADERS;
use reqwest::header::ACCESS_CONTROL_ALLOW_METHODS;
use reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN;
use reqwest::header::ACCESS_CONTROL_MAX_AGE;
use reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS;
use reqwest::header::ACCESS_CONTROL_REQUEST_METHOD;
use reqwest::header::ORIGIN;
use reqwest::redirect::Policy;
use reqwest::Client;
use reqwest::Method;
use reqwest::StatusCode;
use serde_json::json;
use test_log::test;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;
use tower::service_fn;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

pub(crate) use super::axum_http_server_factory::make_axum_router;
use super::*;
use crate::configuration::cors::Cors;
use crate::configuration::HealthCheck;
use crate::configuration::Homepage;
use crate::configuration::Sandbox;
use crate::configuration::Supergraph;
use crate::graphql;
use crate::http_server_factory::HttpServerFactory;
use crate::http_server_factory::HttpServerHandle;
use crate::json_ext::Path;
use crate::plugin::test::MockSubgraph;
use crate::query_planner::BridgeQueryPlanner;
use crate::router_factory::create_plugins;
use crate::router_factory::Endpoint;
use crate::router_factory::RouterFactory;
use crate::services::layers::persisted_queries::PersistedQueryLayer;
use crate::services::layers::query_analysis::QueryAnalysisLayer;
use crate::services::layers::static_page::home_page_content;
use crate::services::layers::static_page::sandbox_page_content;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router_service;
use crate::services::router_service::RouterCreator;
use crate::services::supergraph;
use crate::services::HasSchema;
use crate::services::PluggableSupergraphServiceBuilder;
use crate::services::RouterRequest;
use crate::services::RouterResponse;
use crate::services::SupergraphResponse;
use crate::services::MULTIPART_DEFER_CONTENT_TYPE;
use crate::test_harness::http_client;
use crate::test_harness::http_client::MaybeMultipart;
use crate::uplink::license_enforcement::LicenseState;
use crate::ApolloRouterError;
use crate::Configuration;
use crate::Context;
use crate::ListenAddr;
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
    pub(super) RouterService {
        fn service_call(&mut self, req: RouterRequest) -> impl Future<Output = Result<RouterResponse, BoxError>> + Send + 'static;
    }
}

type MockRouterServiceType = tower_test::mock::Mock<RouterRequest, RouterResponse>;

#[derive(Clone)]
struct TestRouterFactory {
    inner: MockRouterServiceType,
}

impl ServiceFactory<router::Request> for TestRouterFactory {
    type Service = MockRouterServiceType;

    fn create(&self) -> Self::Service {
        self.inner.clone()
    }
}

impl RouterFactory for TestRouterFactory {
    type RouterService = MockRouterServiceType;

    type Future = <<TestRouterFactory as ServiceFactory<router::Request>>::Service as Service<
        router::Request,
    >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        MultiMap::new()
    }
}

async fn init(
    mut mock: impl Service<
            router::Request,
            Response = router::Response,
            Error = BoxError,
            Future = BoxFuture<'static, router::ServiceResult>,
        > + Send
        + 'static,
) -> (HttpServerHandle, Client) {
    let server_factory = AxumHttpServerFactory::new();
    let (service, mut handle) = tower_test::mock::spawn();

    tokio::spawn(async move {
        loop {
            while let Some((request, responder)) = handle.next_request().await {
                match mock.ready().await.unwrap().call(request).await {
                    Ok(response) => responder.send_response(response),
                    Err(err) => responder.send_error(err),
                }
            }
        }
    });
    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

    let server = server_factory
        .create(
            TestRouterFactory {
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
            LicenseState::Unlicensed,
            all_connections_stopped_sender,
        )
        .await
        .expect("Failed to create server factory");
    let mut default_headers = HeaderMap::new();
    default_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(APPLICATION_JSON.essence_str()),
    );
    default_headers.insert(
        ACCEPT,
        HeaderValue::from_static(APPLICATION_JSON.essence_str()),
    );

    let client = reqwest::Client::builder()
        .default_headers(default_headers)
        .redirect(Policy::none())
        .build()
        .unwrap();
    (server, client)
}

pub(super) async fn init_with_config(
    mut router_service: impl Service<
            router::Request,
            Response = router::Response,
            Error = BoxError,
            Future = BoxFuture<'static, router::ServiceResult>,
        > + Send
        + 'static,
    conf: Arc<Configuration>,
    web_endpoints: MultiMap<ListenAddr, Endpoint>,
) -> Result<(HttpServerHandle, Client), ApolloRouterError> {
    let server_factory = AxumHttpServerFactory::new();
    let (service, mut handle) = tower_test::mock::spawn();
    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        loop {
            while let Some((request, responder)) = handle.next_request().await {
                match router_service.ready().await.unwrap().call(request).await {
                    Ok(response) => responder.send_response(response),
                    Err(err) => responder.send_error(err),
                }
            }
        }
    });
    let server = server_factory
        .create(
            TestRouterFactory {
                inner: service.into_inner(),
            },
            conf,
            None,
            vec![],
            web_endpoints,
            LicenseState::Unlicensed,
            all_connections_stopped_sender,
        )
        .await?;
    let mut default_headers = HeaderMap::new();
    default_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(APPLICATION_JSON.essence_str()),
    );
    default_headers.insert(
        ACCEPT,
        HeaderValue::from_static(APPLICATION_JSON.essence_str()),
    );

    let client = reqwest::Client::builder()
        .default_headers(default_headers)
        .redirect(Policy::none())
        .build()
        .unwrap();
    Ok((server, client))
}

#[cfg(unix)]
async fn init_unix(
    mut mock: impl Service<
            router::Request,
            Response = router::Response,
            Error = BoxError,
            Future = BoxFuture<'static, router::ServiceResult>,
        > + Send
        + 'static,
    temp_dir: &tempfile::TempDir,
) -> HttpServerHandle {
    let server_factory = AxumHttpServerFactory::new();
    let (service, mut handle) = tower_test::mock::spawn();
    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        loop {
            while let Some((request, responder)) = handle.next_request().await {
                match mock.ready().await.unwrap().call(request).await {
                    Ok(response) => responder.send_response(response),
                    Err(err) => responder.send_error(err),
                }
            }
        }
    });

    server_factory
        .create(
            TestRouterFactory {
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
            LicenseState::Unlicensed,
            all_connections_stopped_sender,
        )
        .await
        .expect("Failed to create server factory")
}

#[tokio::test]
async fn it_displays_sandbox() {
    let conf = Arc::new(
        Configuration::fake_builder()
            .sandbox(Sandbox::fake_builder().enabled(true).build())
            .homepage(Homepage::fake_builder().enabled(false).build())
            .supergraph(Supergraph::fake_builder().introspection(true).build())
            .build()
            .unwrap(),
    );

    let router_service = router_service::from_supergraph_mock_callback_and_configuration(
        move |_| {
            panic!("this should never be called");
        },
        conf.clone(),
    )
    .await;

    let (server, client) = init_with_config(router_service, conf, MultiMap::new())
        .await
        .unwrap();

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
    assert_eq!(response.text().await.unwrap(), sandbox_page_content());
}

#[tokio::test]
async fn it_displays_sandbox_with_different_supergraph_path() {
    let conf = Arc::new(
        Configuration::fake_builder()
            .sandbox(Sandbox::fake_builder().enabled(true).build())
            .homepage(Homepage::fake_builder().enabled(false).build())
            .supergraph(
                Supergraph::fake_builder()
                    .introspection(true)
                    .path("/custom")
                    .build(),
            )
            .build()
            .unwrap(),
    );

    let router_service = router_service::from_supergraph_mock_callback_and_configuration(
        move |_| {
            panic!("this should never be called");
        },
        conf.clone(),
    )
    .await;
    let (server, client) = init_with_config(router_service, conf, MultiMap::new())
        .await
        .unwrap();

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
    assert_eq!(response.text().await.unwrap(), sandbox_page_content());
}

#[tokio::test]
async fn it_compress_response_body() -> Result<(), ApolloRouterError> {
    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yayyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"})) // Body must be bigger than 32 to be compressed
        .build();
    let example_response = expected_response.clone();
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();

        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
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
    decoder.write_all(&body_bytes).await.unwrap();
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
        Some(&HeaderValue::from_static(APPLICATION_JSON.essence_str()))
    );
    assert_eq!(
        response.headers().get(&CONTENT_ENCODING),
        Some(&HeaderValue::from_static("gzip"))
    );

    // Decompress body
    let body_bytes = response.bytes().await.unwrap();
    let mut decoder = GzipDecoder::new(Vec::new());
    decoder.write_all(&body_bytes).await.unwrap();
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
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        assert_eq!(req.supergraph_request.into_body().query.unwrap(), "query");
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
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
    let (server, client) = init(router_service::empty().await).await;

    let response = client
        .post(format!(
            "{}/",
            server.graphql_listen_address().as_ref().unwrap()
        ))
        .body("Garbage")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        &HeaderValue::from_static("application/json")
    );
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_json: serde_json::Value = response.json().await.unwrap();

    assert_eq!(response_json.get("data"), None);
    let error = &response_json.get("errors").unwrap()[0];
    let error_message = error.get("message").unwrap().as_str().unwrap();
    let extensions = error.get("extensions").unwrap().as_object().unwrap();
    let error_code = extensions.get("code").unwrap().as_str().unwrap();
    let error_details = extensions.get("details").unwrap().as_str().unwrap();
    assert_eq!(error_code, "INVALID_GRAPHQL_REQUEST");
    assert_eq!(
        error_details,
        "failed to deserialize the request body into JSON: expected value at line 1 column 1"
    );
    assert_eq!(error_message, "Invalid GraphQL request");
    server.shutdown().await
}

#[tokio::test]
async fn response() -> Result<(), ApolloRouterError> {
    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yay"}))
        .build();
    let example_response = expected_response.clone();
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();

        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
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
        Some(&HeaderValue::from_static(APPLICATION_JSON.essence_str()))
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
    let (server, client) = init(router_service::empty().await).await;
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
async fn response_with_root_wildcard() -> Result<(), ApolloRouterError> {
    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yay"}))
        .build();
    let example_response = expected_response.clone();

    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;

    let conf = Configuration::fake_builder()
        .supergraph(
            crate::configuration::Supergraph::fake_builder()
                .path(String::from("/*"))
                .build(),
        )
        .build()
        .unwrap();
    let (server, client) =
        init_with_config(router_service, Arc::new(conf), MultiMap::new()).await?;
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

    // Post query without path
    let response = client
        .post(
            &server
                .graphql_listen_address()
                .as_ref()
                .unwrap()
                .to_string(),
        )
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
async fn response_with_custom_endpoint() -> Result<(), ApolloRouterError> {
    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yay"}))
        .build();
    let example_response = expected_response.clone();

    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;

    let conf = Configuration::fake_builder()
        .supergraph(
            crate::configuration::Supergraph::fake_builder()
                .path(String::from("/graphql"))
                .build(),
        )
        .build()
        .unwrap();
    let (server, client) =
        init_with_config(router_service, Arc::new(conf), MultiMap::new()).await?;
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
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;

    let conf = Configuration::fake_builder()
        .supergraph(
            crate::configuration::Supergraph::fake_builder()
                .path(String::from("/:my_prefix/graphql"))
                .build(),
        )
        .build()
        .unwrap();
    let (server, client) =
        init_with_config(router_service, Arc::new(conf), MultiMap::new()).await?;
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

    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;

    let conf = Configuration::fake_builder()
        .supergraph(
            crate::configuration::Supergraph::fake_builder()
                .path(String::from("/graphql/*"))
                .build(),
        )
        .build()
        .unwrap();
    let (server, client) =
        init_with_config(router_service, Arc::new(conf), MultiMap::new()).await?;
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
async fn response_failure() -> Result<(), ApolloRouterError> {
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = crate::error::FetchError::SubrequestHttpError {
            status_code: Some(200),
            service: "Mock service".to_string(),
            reason: "Mock error".to_string(),
        }
        .to_response();

        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;

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
            status_code: Some(200),
            service: "Mock service".to_string(),
            reason: "Mock error".to_string(),
        }
        .to_response()
    );
    server.shutdown().await
}

#[tokio::test]
async fn cors_preflight() -> Result<(), ApolloRouterError> {
    let conf = Configuration::fake_builder()
        .cors(Cors::builder().build())
        .supergraph(
            crate::configuration::Supergraph::fake_builder()
                .path(String::from("/graphql"))
                .build(),
        )
        .build()
        .unwrap();
    let (server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await?;

    let response = client
        .request(
            Method::OPTIONS,
            &format!(
                "{}/graphql",
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
async fn test_previous_health_check_returns_four_oh_four() {
    let (server, client) = init(router_service::empty().await).await;
    let url = format!(
        "{}/.well-known/apollo/server-health",
        server.graphql_listen_address().as_ref().unwrap()
    );

    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test(tokio::test)]
async fn it_errors_on_bad_content_type_header() -> Result<(), ApolloRouterError> {
    let query = "query";
    let operation_name = "operationName";

    let router_service = router_service::from_supergraph_mock_callback(|req| {
        Ok(SupergraphResponse::new_from_graphql_response(
            graphql::Response::builder()
                .data(json!({"response": "hey"}))
                .build(),
            req.context,
        ))
    })
    .await;

    let (server, client) = init(router_service).await;
    let url = format!("{}", server.graphql_listen_address().as_ref().unwrap());
    let response = client
        .post(url.as_str())
        .header(CONTENT_TYPE, "application/yaml")
        .body(json!({ "query": query, "operationName": operation_name }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE,);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static(APPLICATION_JSON.essence_str()))
    );
    assert_eq!(
        response.text().await.unwrap(),
        r#"{"errors":[{"message":"'content-type' header must be one of: \"application/json\" or \"application/graphql-response+json\"","extensions":{"code":"INVALID_CONTENT_TYPE_HEADER"}}]}"#
    );

    server.shutdown().await
}

#[test(tokio::test)]
async fn it_errors_on_bad_accept_header() -> Result<(), ApolloRouterError> {
    let query = "query";
    let operation_name = "operationName";

    let router_service = router_service::from_supergraph_mock_callback(|req| {
        Ok(SupergraphResponse::new_from_graphql_response(
            graphql::Response::builder()
                .data(json!({"response": "hey"}))
                .build(),
            req.context,
        ))
    })
    .await;

    let (server, client) = init(router_service).await;
    let url = format!("{}", server.graphql_listen_address().as_ref().unwrap());
    let response = client
        .post(url.as_str())
        .header(ACCEPT, "foo/bar")
        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
        .body(json!({ "query": query, "operationName": operation_name }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static(APPLICATION_JSON.essence_str()))
    );
    assert_eq!(
        response.text().await.unwrap(),
        r#"{"errors":[{"message":"'accept' header must be one of: \\\"*/*\\\", \"application/json\", \"application/graphql-response+json\", \"multipart/mixed;boundary=\\\"graphql\\\";subscriptionSpec=1.0\" or \"multipart/mixed;boundary=\\\"graphql\\\";deferSpec=20220824\"","extensions":{"code":"INVALID_ACCEPT_HEADER"}}]}"#
    );

    server.shutdown().await
}

#[test(tokio::test)]
async fn it_displays_homepage() {
    let conf = Arc::new(Configuration::fake_builder().build().unwrap());

    let router_service = router_service::from_supergraph_mock_callback_and_configuration(
        |req| {
            Ok(SupergraphResponse::new_from_graphql_response(
                graphql::Response::builder()
                    .data(json!({"response": "test"}))
                    .build(),
                req.context,
            ))
        },
        conf.clone(),
    )
    .await;

    let (server, client) = init_with_config(router_service, conf, MultiMap::new())
        .await
        .unwrap();
    let response = client
        .get(&format!(
            "{}/",
            server.graphql_listen_address().as_ref().unwrap()
        ))
        .header(ACCEPT, "text/html")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.text().await.unwrap(),
        home_page_content(Homepage::fake_builder().enabled(false).build())
    );
    server.shutdown().await.unwrap();
}

#[test(tokio::test)]
async fn it_doesnt_display_disabled_homepage() {
    let conf = Arc::new(
        Configuration::fake_builder()
            .homepage(
                crate::configuration::Homepage::fake_builder()
                    .enabled(false)
                    .build(),
            )
            .build()
            .unwrap(),
    );

    let router_service = router_service::from_supergraph_mock_callback_and_configuration(
        |req| {
            Ok(SupergraphResponse::new_from_graphql_response(
                graphql::Response::builder()
                    .data(json!({"response": "test"}))
                    .build(),
                req.context,
            ))
        },
        conf.clone(),
    )
    .await;

    let (server, client) = init_with_config(router_service, conf, MultiMap::new())
        .await
        .unwrap();
    let response = client
        .get(&format!(
            "{}/",
            server.graphql_listen_address().as_ref().unwrap()
        ))
        .header(ACCEPT, "text/html")
        .header(ACCEPT, "*/*")
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "{:?}",
        response.text().await
    );

    server.shutdown().await.unwrap();
}

#[test(tokio::test)]
async fn it_answers_to_custom_endpoint() -> Result<(), ApolloRouterError> {
    let endpoint = service_fn(|req: router::Request| async move {
        Ok::<_, BoxError>(
            http::Response::builder()
                .status(StatusCode::OK)
                .body(
                    format!(
                        "{} + {}",
                        req.router_request.method(),
                        req.router_request.uri().path()
                    )
                    .into(),
                )
                .unwrap()
                .into(),
        )
    })
    .boxed_clone();
    let mut web_endpoints = MultiMap::new();
    web_endpoints.insert(
        ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
        Endpoint::from_router_service("/a-custom-path".to_string(), endpoint.clone().boxed()),
    );
    web_endpoints.insert(
        ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
        Endpoint::from_router_service("/an-other-custom-path".to_string(), endpoint.boxed()),
    );

    let conf = Configuration::fake_builder().build().unwrap();
    let (server, client) =
        init_with_config(router_service::empty().await, Arc::new(conf), web_endpoints).await?;

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
        assert_eq!(response.text().await.unwrap(), format!("GET + {path}"));
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
        assert_eq!(response.text().await.unwrap(), format!("POST + {path}"));
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
    let endpoint = service_fn(|req: router::Request| async move {
        Ok::<_, BoxError>(
            http::Response::builder()
                .status(StatusCode::OK)
                .body(
                    format!(
                        "{} + {}",
                        req.router_request.method(),
                        req.router_request.uri().path()
                    )
                    .into(),
                )
                .unwrap()
                .into(),
        )
    })
    .boxed_clone();

    let mut web_endpoints = MultiMap::new();
    web_endpoints.insert(
        ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
        Endpoint::from_router_service("/a-custom-path".to_string(), endpoint.clone().boxed()),
    );
    web_endpoints.insert(
        ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()),
        Endpoint::from_router_service("/a-custom-path".to_string(), endpoint.boxed()),
    );

    let conf = Configuration::fake_builder().build().unwrap();
    let error = init_with_config(router_service::empty().await, Arc::new(conf), web_endpoints)
        .await
        .unwrap_err();

    assert_eq!(
        "tried to register two endpoints on `127.0.0.1:0/a-custom-path`",
        error.to_string()
    )
}

#[tokio::test]
async fn cors_origin_default() -> Result<(), ApolloRouterError> {
    let (server, client) = init(router_service::empty().await).await;
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

    let response =
        request_cors_with_origin(&client, url.as_str(), "https://studio.apollographql.com").await;
    assert_cors_origin(response, "https://studio.apollographql.com");

    let response =
        request_cors_with_origin(&client, url.as_str(), "https://this.wont.work.com").await;
    assert_not_cors_origin(response, "https://this.wont.work.com");
    Ok(())
}

#[tokio::test]
async fn cors_max_age() -> Result<(), ApolloRouterError> {
    let conf = Configuration::fake_builder()
        .cors(Cors::builder().max_age(Duration::from_secs(100)).build())
        .build()
        .unwrap();
    let (server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await?;
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

    let response = request_cors_with_origin(&client, url.as_str(), "https://thisisatest.com").await;
    assert_cors_max_age(response, "100");

    Ok(())
}

#[tokio::test]
async fn cors_allow_any_origin() -> Result<(), ApolloRouterError> {
    let conf = Configuration::fake_builder()
        .cors(Cors::builder().allow_any_origin(true).build())
        .build()
        .unwrap();
    let (server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await?;
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

    let response = request_cors_with_origin(&client, url.as_str(), "https://thisisatest.com").await;
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
    let (server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await?;
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
    let (server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await?;
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());

    // regex tests
    let response =
        request_cors_with_origin(&client, url.as_str(), "https://www.apollographql.com").await;
    assert_cors_origin(response, "https://www.apollographql.com");
    let response =
        request_cors_with_origin(&client, url.as_str(), "https://staging.apollographql.com").await;
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

async fn request_cors_with_origin(client: &Client, url: &str, origin: &str) -> reqwest::Response {
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

fn assert_cors_max_age(response: reqwest::Response, max_age: &str) {
    assert!(response.status().is_success());
    assert_headers_valid(&response);
    assert_header_contains!(response, ACCESS_CONTROL_MAX_AGE, &[max_age]);
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
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        Ok(SupergraphResponse::new_from_graphql_response(
            graphql::Response::builder()
                .data(json!({
                    "test": "hello"
                }))
                .build(),
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
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

    println!("response: {response:?}");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static(APPLICATION_JSON.essence_str()))
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
    let router_service = router_service::from_supergraph_mock_callback(|req| {
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
        Ok(SupergraphResponse::new_from_response(
            http::Response::builder().status(200).body(body).unwrap(),
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
    let query = json!(
    {
      "query": "query { test ... @defer { other } }",
    });
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());
    let mut response = client
        .post(&url)
        .body(query.to_string())
        .header(
            ACCEPT,
            HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE))
    );

    let first = response.chunk().await.unwrap().unwrap();
    assert_eq!(
            std::str::from_utf8(&first).unwrap(),
            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"data\":{\"test\":\"hello\"},\"hasNext\":true}\r\n--graphql\r\n"
        );

    let second = response.chunk().await.unwrap().unwrap();
    assert_eq!(
            std::str::from_utf8(&second).unwrap(),
        "content-type: application/json\r\n\r\n{\"hasNext\":true,\"incremental\":[{\"data\":{\"other\":\"world\"},\"path\":[]}]}\r\n--graphql\r\n"
        );

    let third = response.chunk().await.unwrap().unwrap();
    assert_eq!(
        std::str::from_utf8(&third).unwrap(),
        "content-type: application/json\r\n\r\n{\"hasNext\":false}\r\n--graphql--\r\n"
    );

    server.shutdown().await
}

#[test(tokio::test)]
async fn multipart_response_shape_with_one_chunk() -> Result<(), ApolloRouterError> {
    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let body = stream::iter(vec![graphql::Response::builder()
            .data(json!({
                "test": "hello",
            }))
            .has_next(false)
            .build()])
        .boxed();

        Ok(SupergraphResponse::new_from_response(
            http::Response::builder().status(200).body(body).unwrap(),
            req.context,
        ))
    })
    .await;
    let (server, client) = init(router_service).await;
    let query = json!(
    {
      "query": "query { test }",
    });
    let url = format!("{}/", server.graphql_listen_address().as_ref().unwrap());
    let mut response = client
        .post(&url)
        .body(query.to_string())
        .header(
            ACCEPT,
            HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE))
    );

    let first = response.chunk().await.unwrap().unwrap();
    assert_eq!(
            std::str::from_utf8(&first).unwrap(),
            "\r\n--graphql\r\ncontent-type: application/json\r\n\r\n{\"data\":{\"test\":\"hello\"},\"hasNext\":false}\r\n--graphql--\r\n"
        );

    server.shutdown().await
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

    let supergraph_service_factory = TestRouterFactory {
        inner: service.into_inner(),
    };

    let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

    let server = server_factory
        .create(
            supergraph_service_factory.clone(),
            configuration,
            None,
            vec![],
            MultiMap::new(),
            LicenseState::default(),
            all_connections_stopped_sender,
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
            LicenseState::default(),
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

enum RequestType {
    Compressed,
    Deferred,
}

async fn http_compressed_service() -> impl Service<
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

    let service = http_client::response_decompression(service)
        .map_request(|mut req: http::Request<hyper::Body>| {
            req.headers_mut().append(
                ACCEPT,
                HeaderValue::from_static(APPLICATION_JSON.essence_str()),
            );
            req
        })
        .map_future(|future| async {
            let response: http::Response<Pin<Box<dyn AsyncRead + Send>>> = future.await?;
            let (parts, mut body) = response.into_parts();

            let mut vec = Vec::new();
            body.read_to_end(&mut vec).await.unwrap();
            let body = MaybeMultipart::NotMultipart(vec);
            Ok(http::Response::from_parts(parts, body))
        });
    http_client::json(service)
}

async fn http_deferred_service() -> impl Service<
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
        .map_err(Into::into)
        .map_response(|response: http::Response<BoxBody>| {
            let response = response.map(|body| {
                // Convert from axum’s BoxBody to AsyncBufRead
                let mut body = Box::pin(body);
                let stream = poll_fn(move |ctx| body.as_mut().poll_data(ctx))
                    .map(|result| result.map_err(|e| io::Error::new(io::ErrorKind::Other, e)));
                StreamReader::new(stream)
            });
            response.map(|body| Box::pin(body) as _)
        });

    let service = http_client::defer_spec_20220824_multipart(service);

    http_client::json(service)
}

/// Creates an Apollo Router as an HTTP-level Tower service and makes one request.
async fn make_request(
    request_body: serde_json::Value,
    request_type: RequestType,
) -> http::Response<MaybeMultipart<serde_json::Value>> {
    let request = http::Request::builder()
        .method(http::Method::POST)
        .header("host", "127.0.0.1")
        .body(request_body)
        .unwrap();
    match request_type {
        RequestType::Compressed => http_compressed_service()
            .await
            .oneshot(request)
            .await
            .unwrap(),
        RequestType::Deferred => http_deferred_service()
            .await
            .oneshot(request)
            .await
            .unwrap(),
    }
}

fn assert_compressed<B>(response: &http::Response<B>, expected: bool) {
    assert_eq!(
        response
            .extensions()
            .get::<http_client::ResponseBodyWasCompressed>()
            .map(|e| e.0)
            .unwrap_or_default(),
        expected
    )
}

#[tokio::test]
async fn test_compressed_response() {
    let response = make_request(
        json!({
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
        }),
        RequestType::Compressed,
    )
    .await;
    assert_compressed(&response, true);
    let status = response.status().as_u16();
    let graphql_response = response.into_body().expect_not_multipart();
    assert_eq!(graphql_response["errors"], json!(null));
    assert_eq!(status, 200);
}

#[tokio::test]
async fn test_defer_is_not_buffered() {
    let mut response = make_request(
        json!({
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
        }),
        RequestType::Deferred,
    )
    .await;
    assert_compressed(&response, false);
    let status = response.status().as_u16();
    assert_eq!(status, 200);
    let counter: GraphQLResponseCounter = response.extensions_mut().remove().unwrap();
    let parts = response.into_body().expect_multipart();

    let (parts, counts): (Vec<_>, Vec<_>) = parts.map(|part| (part, counter.get())).unzip().await;
    let parts = serde_json::Value::Array(parts);
    insta::assert_json_snapshot!(parts);

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

#[tokio::test]
#[cfg(unix)]
async fn listening_to_unix_socket() {
    let temp_dir = tempfile::tempdir().unwrap();
    let expected_response = graphql::Response::builder()
        .data(json!({"response": "yay"}))
        .build();
    let example_response = expected_response.clone();

    let router_service = router_service::from_supergraph_mock_callback(move |req| {
        let example_response = example_response.clone();
        Ok(SupergraphResponse::new_from_graphql_response(
            example_response,
            req.context,
        ))
    })
    .await;
    let server = init_unix(router_service, &temp_dir).await;

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
Accept: application/json\r

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
Accept: application/json\r

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
async fn test_health_check() {
    let router_service = router_service::from_supergraph_mock_callback(|_| {
        Ok(supergraph::Response::builder()
            .data(json!({ "__typename": "Query"}))
            .context(Context::new())
            .build()
            .unwrap())
    })
    .await;

    let (server, client) = init(router_service).await;
    let url = format!(
        "{}/health",
        server.graphql_listen_address().as_ref().unwrap()
    );

    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json!({"status": "UP" }),
        response.json::<serde_json::Value>().await.unwrap()
    )
}

#[tokio::test]
async fn test_health_check_custom_listener() {
    let conf = Configuration::fake_builder()
        .health_check(
            HealthCheck::fake_builder()
                .listen(ListenAddr::SocketAddr("127.0.0.1:4012".parse().unwrap()))
                .enabled(true)
                .build(),
        )
        .build()
        .unwrap();

    // keep the server handle around otherwise it will immediately shutdown
    let (_server, client) = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await
    .unwrap();
    let url = "http://localhost:4012/health";

    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json!({"status": "UP" }),
        response.json::<serde_json::Value>().await.unwrap()
    )
}

#[tokio::test]
async fn test_sneaky_supergraph_and_health_check_configuration() {
    let conf = Configuration::fake_builder()
        .health_check(
            HealthCheck::fake_builder()
                .listen(ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()))
                .enabled(true)
                .build(),
        )
        .supergraph(Supergraph::fake_builder().path("/health").build()) // here be dragons
        .build()
        .unwrap();
    let error = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await
    .unwrap_err();

    assert_eq!(
        "tried to register two endpoints on `127.0.0.1:0/health`",
        error.to_string()
    );
}

#[tokio::test]
async fn test_sneaky_supergraph_and_disabled_health_check_configuration() {
    let conf = Configuration::fake_builder()
        .health_check(
            HealthCheck::fake_builder()
                .listen(ListenAddr::SocketAddr("127.0.0.1:0".parse().unwrap()))
                .enabled(false)
                .build(),
        )
        .supergraph(Supergraph::fake_builder().path("/health").build())
        .build()
        .unwrap();
    let _ = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_supergraph_and_health_check_same_port_different_listener() {
    let conf = Configuration::fake_builder()
        .health_check(
            HealthCheck::fake_builder()
                .listen(ListenAddr::SocketAddr("127.0.0.1:4013".parse().unwrap()))
                .enabled(true)
                .build(),
        )
        .supergraph(
            Supergraph::fake_builder()
                .listen(ListenAddr::SocketAddr("0.0.0.0:4013".parse().unwrap()))
                .build(),
        )
        .build()
        .unwrap();
    let error = init_with_config(
        router_service::empty().await,
        Arc::new(conf),
        MultiMap::new(),
    )
    .await
    .unwrap_err();

    assert_eq!(
        "tried to bind 0.0.0.0 and 127.0.0.1 on port 4013",
        error.to_string()
    );
}

#[tokio::test]
async fn test_supergraph_timeout() {
    let config = serde_json::json!({
        "supergraph": {
            "defer_support": false,
        },
        "traffic_shaping": {
            "router": {
                "timeout": "1ns"
            }
        },
    });

    let conf: Arc<Configuration> = Arc::new(serde_json::from_value(config).unwrap());

    let schema = include_str!("..//testdata/minimal_supergraph.graphql");
    let planner = BridgeQueryPlanner::new(schema.to_string(), conf.clone())
        .await
        .unwrap();
    let schema = planner.schema();

    // we do the entire supergraph rebuilding instead of using `from_supergraph_mock_callback_and_configuration`
    // because we need the plugins to apply on the supergraph
    let plugins = create_plugins(&conf, &schema, None).await.unwrap();

    let mut builder = PluggableSupergraphServiceBuilder::new(planner)
        .with_configuration(conf.clone())
        .with_subgraph_service("accounts", MockSubgraph::new(HashMap::new()));

    for (name, plugin) in plugins.into_iter() {
        builder = builder.with_dyn_plugin(name, plugin);
    }
    let supergraph_creator = builder.build().await.unwrap();

    let service = RouterCreator::new(
        QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&conf)).await,
        Arc::new(PersistedQueryLayer::new(&conf).await.unwrap()),
        Arc::new(supergraph_creator),
        conf.clone(),
    )
    .await
    .unwrap()
    .make();

    // keep the server handle around otherwise it will immediately shutdown
    let (_server, client) = init_with_config(service, conf.clone(), MultiMap::new())
        .await
        .unwrap();
    let url = "http://localhost:4000/";

    let response = client
        .post(url)
        .body(r#"{ "query": "{ me }" }"#)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = response.bytes().await.unwrap();
    assert_eq!(std::str::from_utf8(&body).unwrap(), "request timed out");
}
