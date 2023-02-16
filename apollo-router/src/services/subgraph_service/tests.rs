use std::convert::Infallible;
use std::net::SocketAddr;
use std::str::FromStr;

use axum::Server;
use bytes::Buf;
use http::header::HOST;
use http::StatusCode;
use http::Uri;
use hyper::service::make_service_fn;
use hyper::Body;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tower::service_fn;
use tower::ServiceExt;
use SubgraphRequest;

use super::*;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::query_planner::fetch::OperationKind;
use crate::Context;

// starts a local server emulating a subgraph returning status code 400
async fn emulate_subgraph_bad_request(socket_addr: SocketAddr) {
    async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        Ok(http::Response::builder()
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .status(StatusCode::BAD_REQUEST)
            .body(
                serde_json::to_string(&Response {
                    errors: vec![Error::builder()
                        .message("This went wrong")
                        .extension_code("FETCH_ERROR")
                        .build()],
                    ..Response::default()
                })
                .expect("always valid")
                .into(),
            )
            .unwrap())
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning status code 401
async fn emulate_subgraph_unauthorized(socket_addr: SocketAddr) {
    async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        Ok(http::Response::builder()
            .header(CONTENT_TYPE, "text/html")
            .status(StatusCode::UNAUTHORIZED)
            .body(r#""#.into())
            .unwrap())
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning bad response format
async fn emulate_subgraph_bad_response_format(socket_addr: SocketAddr) {
    async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        Ok(http::Response::builder()
            .header(CONTENT_TYPE, "text/html")
            .status(StatusCode::OK)
            .body(r#"TEST"#.into())
            .unwrap())
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning compressed response
async fn emulate_subgraph_compressed_response(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        // Check the compression of the body
        let mut encoder = GzipEncoder::new(Vec::new());
        encoder
            .write_all(
                &serde_json::to_vec(&Request::builder().query("query".to_string()).build())
                    .unwrap(),
            )
            .await
            .unwrap();
        encoder.shutdown().await.unwrap();
        let compressed_body = encoder.into_inner();
        assert_eq!(
            compressed_body,
            hyper::body::to_bytes(request.into_body())
                .await
                .unwrap()
                .to_vec()
        );

        let original_body = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };
        let mut encoder = GzipEncoder::new(Vec::new());
        encoder
            .write_all(&serde_json::to_vec(&original_body).unwrap())
            .await
            .unwrap();
        encoder.shutdown().await.unwrap();
        let compressed_body = encoder.into_inner();

        Ok(http::Response::builder()
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .header(CONTENT_ENCODING, "gzip")
            .status(StatusCode::OK)
            .body(compressed_body.into())
            .unwrap())
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning response with
// "errors" : {["message": "PersistedQueryNotSupported",...],...}
async fn emulate_persisted_query_not_supported_message(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");
        match graphql_request {
            Ok(request) => {
                if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                errors: vec![Error::builder()
                                    .message(PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE)
                                    .extension_code("Random code")
                                    .build()],
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }

                return Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .expect("always valid")
                        .into(),
                    )
                    .unwrap());
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning response with
// "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_SUPPORTED"}],...}
async fn emulate_persisted_query_not_supported_extension_code(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");
        match graphql_request {
            Ok(request) => {
                if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                errors: vec![Error::builder()
                                    .message("Random message")
                                    .extension_code(PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE)
                                    .build()],
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }

                return Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .expect("always valid")
                        .into(),
                    )
                    .unwrap());
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning response with
// "errors" : {["message": "PersistedQueryNotFound",...],...}
async fn emulate_persisted_query_not_found_message(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");

        match graphql_request {
            Ok(request) => {
                if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    panic!("Recieved request without persisted query in persisted_query_not_found test.")
                }

                if request.query.is_none() {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                errors: vec![Error::builder()
                                    .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                                    .extension_code("Random Code")
                                    .build()],
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                } else {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning response with
// "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}],...}
async fn emulate_persisted_query_not_found_extension_code(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");

        match graphql_request {
            Ok(request) => {
                if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    panic!("Recieved request without persisted query in persisted_query_not_found test.")
                }

                if request.query.is_none() {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                errors: vec![Error::builder()
                                    .message("Random message")
                                    .extension_code(PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE)
                                    .build()],
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                } else {
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(
                            serde_json::to_string(&Response {
                                data: Some(Value::String(ByteString::from("test"))),
                                ..Response::default()
                            })
                            .expect("always valid")
                            .into(),
                        )
                        .unwrap());
                }
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning a response to request with apq
// and panics if it does not find a persistedQuery.
async fn emulate_expected_apq_enabled_configuration(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");

        match graphql_request {
            Ok(request) => {
                if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    panic!("persistedQuery expected when configuration has apq_enabled=true")
                }

                return Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .expect("always valid")
                        .into(),
                    )
                    .unwrap());
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

// starts a local server emulating a subgraph returning a response to request without apq
// and panics if it finds a persistedQuery.
async fn emulate_expected_apq_disabled_configuration(socket_addr: SocketAddr) {
    async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let (_, body) = request.into_parts();
        let graphql_request: Result<graphql::Request, &str> = hyper::body::to_bytes(body)
            .await
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
            .map_err(|_| "failed to parse the request body as JSON");

        match graphql_request {
            Ok(request) => {
                if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                    panic!("persistedQuery not expected when configuration has apq_enabled=false")
                }

                return Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .expect("always valid")
                        .into(),
                    )
                    .unwrap());
            }
            Err(_) => {
                panic!("invalid graphql request recieved")
            }
        }
    }

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::bind(&socket_addr).serve(make_svc);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_bad_status_code_should_not_fail() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:2626").unwrap();
    tokio::task::spawn(emulate_subgraph_bad_request(socket_addr));
    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let response = subgraph_service
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        response.response.body().errors[0].message,
        "This went wrong"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_bad_content_type() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:2525").unwrap();
    tokio::task::spawn(emulate_subgraph_bad_response_format(socket_addr));

    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let err = subgraph_service
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "HTTP fetch failed from 'test': subgraph didn't return JSON (expected content-type: application/json or content-type: application/graphql-response+json; found content-type: \"text/html\")"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_compressed_request_response_body() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:2727").unwrap();
    tokio::task::spawn(emulate_subgraph_compressed_response(socket_addr));
    let subgraph_service = SubgraphService::new("test", false, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query".to_string()).build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header(CONTENT_ENCODING, "gzip")
                .uri(url)
                .body(Request::builder().query("query".to_string()).build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();
    // Test the right decompression of the body
    let resp_from_subgraph = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &resp_from_subgraph);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_unauthorized() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:2828").unwrap();
    tokio::task::spawn(emulate_subgraph_unauthorized(socket_addr));
    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let err = subgraph_service
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "HTTP fetch failed from 'test': 401: Unauthorized"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_persisted_query_not_supported_message() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:2929").unwrap();
    tokio::task::spawn(emulate_persisted_query_not_supported_message(socket_addr));
    let subgraph_service = SubgraphService::new("test", true, None);

    assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
    assert!(!subgraph_service.apq.as_ref().load(Relaxed));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_persisted_query_not_supported_extension_code() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:3030").unwrap();
    tokio::task::spawn(emulate_persisted_query_not_supported_extension_code(
        socket_addr,
    ));
    let subgraph_service = SubgraphService::new("test", true, None);

    assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
    assert!(!subgraph_service.apq.as_ref().load(Relaxed));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_persisted_query_not_found_message() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:3131").unwrap();
    tokio::task::spawn(emulate_persisted_query_not_found_message(socket_addr));
    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_persisted_query_not_found_extension_code() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:3232").unwrap();
    tokio::task::spawn(emulate_persisted_query_not_found_extension_code(
        socket_addr,
    ));
    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_apq_enabled_subgraph_configuration() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:3333").unwrap();
    tokio::task::spawn(emulate_expected_apq_enabled_configuration(socket_addr));
    let subgraph_service = SubgraphService::new("test", true, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_apq_disabled_subgraph_configuration() {
    let socket_addr = SocketAddr::from_str("127.0.0.1:3434").unwrap();
    tokio::task::spawn(emulate_expected_apq_disabled_configuration(socket_addr));
    let subgraph_service = SubgraphService::new("test", false, None);

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = subgraph_service
        .clone()
        .oneshot(SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: Context::new(),
        })
        .await
        .unwrap();

    let expected_resp = Response {
        data: Some(Value::String(ByteString::from("test"))),
        ..Response::default()
    };

    assert_eq!(resp.response.body(), &expected_resp);
}
