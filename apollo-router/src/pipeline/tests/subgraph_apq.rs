//! Subgraph APQ enablement per the `apq.subgraph` config, exercised through the full
//! per-subgraph service stack against a real local HTTP listener.

use std::convert::Infallible;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use bytes::Buf;
use http::StatusCode;
use http::Uri;
use http::header::CONTENT_TYPE;
use indexmap::IndexMap;
use mime::APPLICATION_JSON;
use serde_json_bytes::ByteString;
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::Configuration;
use crate::Context;
use crate::configuration::SubgraphApq;
use crate::graphql::Response;
use crate::pipeline::build_subgraph_services;
use crate::query_planner::fetch::OperationKind;
use crate::services::SubgraphRequest;
use crate::services::http::test_http_client_service;
use crate::services::layers::apq::subgraph::PERSISTED_QUERY_KEY;
use crate::services::router;

async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
where
    Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
    Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
{
    use hyper::body::Incoming;
    use hyper_util::rt::TokioExecutor;
    use hyper_util::rt::TokioIo;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handle = handle.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(|request: http::Request<Incoming>| {
                handle(request.map(Body::new))
            });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("server error: {err}");
            }
        });
    }
}

async fn parse_graphql_request(body: Body) -> crate::graphql::Request {
    let bytes = router::body::into_bytes(body)
        .await
        .expect("can read request body");
    serde_json::from_reader(bytes.reader()).expect("valid graphql request")
}

fn subgraph_request(uri: Uri, subgraph_name: &str, query: &str) -> SubgraphRequest {
    SubgraphRequest::builder()
        .supergraph_request(Arc::new(
            http::Request::builder()
                .body(crate::graphql::Request::builder().query(query).build())
                .unwrap(),
        ))
        .subgraph_request(
            http::Request::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(uri)
                .body(crate::graphql::Request::builder().query(query).build())
                .unwrap(),
        )
        .operation_kind(OperationKind::Query)
        .subgraph_name(subgraph_name.to_string())
        .context(Context::new())
        .build()
}

#[tokio::test(flavor = "multi_thread")]
async fn respects_all_enabled_config() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
        serve(listener, |request| async move {
            let graphql_request = parse_graphql_request(request.into_body()).await;
            assert!(graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
            assert!(graphql_request.query.is_none());
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response {
                        data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                        ..Response::default()
                    })
                    .unwrap()
                    .into(),
                )
                .unwrap())
        })
        .await
        .unwrap();
    });

    let mut config = Configuration::default();
    config.apq.subgraph.all.enabled = true;

    let mut http_services = IndexMap::new();
    http_services.insert("test".to_string(), test_http_client_service("test"));

    let services = build_subgraph_services(http_services, &Default::default(), &config);
    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let resp = services
        .get("test")
        .unwrap()
        .oneshot(subgraph_request(url, "test", "query"))
        .await
        .unwrap();

    assert_eq!(
        resp.response.body().data,
        Some(serde_json_bytes::Value::String(ByteString::from("test")))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn respects_all_disabled_config() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
        serve(listener, |request| async move {
            let graphql_request = parse_graphql_request(request.into_body()).await;
            assert!(!graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
            assert!(graphql_request.query.is_some());
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response {
                        data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                        ..Response::default()
                    })
                    .unwrap()
                    .into(),
                )
                .unwrap())
        })
        .await
        .unwrap();
    });

    let mut config = Configuration::default();
    config.apq.subgraph.all.enabled = false;

    let mut http_services = IndexMap::new();
    http_services.insert("test".to_string(), test_http_client_service("test"));

    let services = build_subgraph_services(http_services, &Default::default(), &config);
    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    services
        .get("test")
        .unwrap()
        .oneshot(subgraph_request(url, "test", "query"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn per_subgraph_override_takes_precedence_over_all() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
        serve(listener, |request| async move {
            let subgraph_name = request
                .headers()
                .get("x-subgraph-name")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let graphql_request = parse_graphql_request(request.into_body()).await;

            match subgraph_name.as_str() {
                "enabled_subgraph" => {
                    assert!(graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                    assert!(graphql_request.query.is_none());
                }
                "disabled_subgraph" => {
                    assert!(!graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                    assert!(graphql_request.query.is_some());
                }
                other => panic!("unexpected subgraph name: {other}"),
            }

            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response {
                        data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                        ..Response::default()
                    })
                    .unwrap()
                    .into(),
                )
                .unwrap())
        })
        .await
        .unwrap();
    });

    let mut config = Configuration::default();
    config.apq.subgraph.all.enabled = false;
    config.apq.subgraph.subgraphs.insert(
        "enabled_subgraph".to_string(),
        SubgraphApq { enabled: true },
    );

    let mut http_services = IndexMap::new();
    http_services.insert(
        "enabled_subgraph".to_string(),
        test_http_client_service("enabled_subgraph"),
    );
    http_services.insert(
        "disabled_subgraph".to_string(),
        test_http_client_service("disabled_subgraph"),
    );

    let services = build_subgraph_services(http_services, &Default::default(), &config);
    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();

    let enabled_request = SubgraphRequest::builder()
        .supergraph_request(Arc::new(
            http::Request::builder()
                .body(crate::graphql::Request::builder().query("query").build())
                .unwrap(),
        ))
        .subgraph_request(
            http::Request::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header("x-subgraph-name", "enabled_subgraph")
                .uri(url.clone())
                .body(crate::graphql::Request::builder().query("query").build())
                .unwrap(),
        )
        .operation_kind(OperationKind::Query)
        .subgraph_name("enabled_subgraph".to_string())
        .context(Context::new())
        .build();

    let disabled_request = SubgraphRequest::builder()
        .supergraph_request(Arc::new(
            http::Request::builder()
                .body(crate::graphql::Request::builder().query("query").build())
                .unwrap(),
        ))
        .subgraph_request(
            http::Request::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header("x-subgraph-name", "disabled_subgraph")
                .uri(url)
                .body(crate::graphql::Request::builder().query("query").build())
                .unwrap(),
        )
        .operation_kind(OperationKind::Query)
        .subgraph_name("disabled_subgraph".to_string())
        .context(Context::new())
        .build();

    services
        .get("enabled_subgraph")
        .unwrap()
        .oneshot(enabled_request)
        .await
        .unwrap();
    services
        .get("disabled_subgraph")
        .unwrap()
        .oneshot(disabled_request)
        .await
        .unwrap();
}
