//! Tower fetcher for subgraphs.

use crate::prelude::*;
use ::serde::Deserialize;
use async_compression::tokio::write::{BrotliEncoder, GzipEncoder, ZlibEncoder};
use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::{
    header::{self, ACCEPT, CONTENT_ENCODING, CONTENT_TYPE},
    HeaderMap, HeaderValue, StatusCode,
};
use hyper::client::HttpConnector;
use hyper_rustls::HttpsConnector;
use opentelemetry::{global, trace::SpanKind};
use schemars::JsonSchema;
use std::task::Poll;
use std::{fmt::Display, sync::Arc};
use tokio::io::AsyncWriteExt;
use tower::{BoxError, ServiceBuilder};
use tower_http::decompression::{Decompression, DecompressionLayer};
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// gzip
    Gzip,
    /// deflate
    Deflate,
    /// brotli
    Br,
}

impl Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compression::Gzip => write!(f, "gzip"),
            Compression::Deflate => write!(f, "deflate"),
            Compression::Br => write!(f, "br"),
        }
    }
}

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub struct SubgraphService {
    client: Decompression<hyper::Client<HttpsConnector<HttpConnector>>>,
    service: Arc<String>,
}

impl SubgraphService {
    pub fn new(service: impl Into<String>) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        Self {
            client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(hyper::Client::builder().build(connector)),
            service: Arc::new(service.into()),
        }
    }
}

impl tower::Service<graphql::SubgraphRequest> for SubgraphService {
    type Response = graphql::SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: graphql::SubgraphRequest) -> Self::Future {
        let graphql::SubgraphRequest {
            subgraph_request,
            context,
            ..
        } = request;

        let mut client = self.client.clone();
        let service_name = (*self.service).to_owned();

        Box::pin(async move {
            let (parts, body) = subgraph_request.into_parts();

            let body = serde_json::to_string(&body).expect("JSON serialization should not fail");

            let compressed_body = compress(body, &parts.headers)
                .instrument(tracing::debug_span!("body_compression"))
                .await
                .map_err(|err| {
                    tracing::error!(compress_error = format!("{:?}", err).as_str());

                    graphql::FetchError::CompressionError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            let mut request = http::request::Request::from_parts(parts, compressed_body.into());
            let app_json: HeaderValue = HeaderValue::from_static("application/json");
            let app_graphql_json: HeaderValue =
                HeaderValue::from_static("application/graphql+json");
            request.headers_mut().insert(CONTENT_TYPE, app_json.clone());
            request.headers_mut().insert(ACCEPT, app_json);
            request.headers_mut().append(ACCEPT, app_graphql_json);

            get_text_map_propagator(|propagator| {
                propagator.inject_context(
                    &Span::current().context(),
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            });

            let schema_uri = request.uri();
            let host = schema_uri.host().map(String::from).unwrap_or_default();
            let port = schema_uri.port_u16().unwrap_or_else(|| {
                let scheme = schema_uri.scheme_str();
                if scheme == Some("https") {
                    443
                } else if scheme == Some("http") {
                    80
                } else {
                    0
                }
            });
            let path = schema_uri.path().to_string();
            let response = client
                .call(request)
                .instrument(tracing::info_span!("subgraph_request",
                    "otel.kind" = %SpanKind::Client,
                    "net.peer.name" = &display(host),
                    "net.peer.port" = &display(port),
                    "http.route" = &display(path),
                    "net.transport" = "ip_tcp"
                ))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    graphql::FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            // Keep our parts, we'll need them later
            let (parts, body) = response.into_parts();
            if let Some(content_type) = parts.headers.get(header::CONTENT_TYPE) {
                if let Ok(content_type_str) = content_type.to_str() {
                    // Using .contains because sometimes we could have charset included (example: "application/json; charset=utf-8")
                    if !content_type_str.contains("application/json")
                        && !content_type_str.contains("application/graphql+json")
                    {
                        tracing::error!(
                            "response body : {:?}",
                            String::from_utf8_lossy(
                                &hyper::body::to_bytes(body).await.unwrap().to_vec()
                            )
                        );
                        return Err(BoxError::from(graphql::FetchError::SubrequestHttpError {
                            service: service_name.clone(),
                            reason: format!("subgraph didn't return JSON (expected content-type: application/json or content-type: application/graphql+json; found content-type: {content_type:?})"),
                        }));
                    }
                }
            }

            let body = hyper::body::to_bytes(body)
                .instrument(tracing::debug_span!("aggregate_response_data"))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    graphql::FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;
            if parts.status != StatusCode::OK {
                return Err(BoxError::from(graphql::FetchError::SubrequestHttpError {
                    service: service_name.clone(),
                    reason: format!(
                        "subgraph HTTP status error '{}': {})",
                        parts.status,
                        String::from_utf8_lossy(&body)
                    ),
                }));
            }

            let graphql: graphql::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                        graphql::FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;

            let resp = http::Response::from_parts(parts, graphql);

            Ok(graphql::SubgraphResponse::new_from_response(
                resp.into(),
                context,
            ))
        })
    }
}

pub async fn compress(body: String, headers: &HeaderMap) -> Result<Vec<u8>, BoxError> {
    let content_encoding = headers.get(&CONTENT_ENCODING);
    match content_encoding {
        Some(content_encoding) => match content_encoding.to_str()? {
            "br" => {
                let mut br_encoder = BrotliEncoder::new(Vec::new());
                br_encoder.write_all(body.as_bytes()).await?;
                br_encoder.shutdown().await?;

                Ok(br_encoder.into_inner())
            }
            "gzip" => {
                let mut gzip_encoder = GzipEncoder::new(Vec::new());
                gzip_encoder.write_all(body.as_bytes()).await?;
                gzip_encoder.shutdown().await?;

                Ok(gzip_encoder.into_inner())
            }
            "deflate" => {
                let mut df_encoder = ZlibEncoder::new(Vec::new());
                df_encoder.write_all(body.as_bytes()).await?;
                df_encoder.shutdown().await?;

                Ok(df_encoder.into_inner())
            }
            "identity" => Ok(body.into_bytes()),
            unknown => {
                tracing::error!("unknown content-encoding value '{:?}'", unknown);
                Err(BoxError::from(format!(
                    "unknown content-encoding value '{:?}'",
                    unknown
                )))
            }
        },
        None => Ok(body.into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use axum::Server;
    use http::header::HOST;
    use http::Uri;
    use hyper::service::make_service_fn;
    use hyper::Body;
    use serde_json_bytes::{ByteString, Value};
    use tower::{service_fn, ServiceExt};

    use crate::fetch::OperationKind;
    use crate::{http_compat, Context, Object, Request, Response, SubgraphRequest};

    use super::*;

    // starts a local server emulating a subgraph returning status code 400
    async fn emulate_subgraph_bad_request(socket_addr: SocketAddr) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header("Content-Type", "application/json")
                .status(StatusCode::BAD_REQUEST)
                .body(r#"BAD REQUEST"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        if let Err(e) = server.await {
            eprintln!("server error: {}", e);
        }
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_bad_response_format(socket_addr: SocketAddr) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header("Content-Type", "text/html")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        if let Err(e) = server.await {
            eprintln!("server error: {}", e);
        }
    }

    // starts a local server emulating a subgraph returning compressed response
    async fn emulate_subgraph_compressed_response(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            // Check the compression of the body
            let mut encoder = GzipEncoder::new(Vec::new());
            encoder
                .write_all(
                    &serde_json::to_vec(
                        &Request::builder().query(Some("query".to_string())).build(),
                    )
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
                path: None,
                label: None,
                errors: vec![],
                extensions: Object::new(),
            };
            let mut encoder = GzipEncoder::new(Vec::new());
            encoder
                .write_all(&serde_json::to_vec(&original_body).unwrap())
                .await
                .unwrap();
            encoder.shutdown().await.unwrap();
            let compressed_body = encoder.into_inner();

            Ok(http::Response::builder()
                .header("Content-Type", "application/json")
                .header(CONTENT_ENCODING, "gzip")
                .status(StatusCode::OK)
                .body(compressed_body.into())
                .unwrap())
        }

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
        let server = Server::bind(&socket_addr).serve(make_svc);
        if let Err(e) = server.await {
            eprintln!("server error: {}", e);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2626").unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(socket_addr));
        let subgraph_service = SubgraphService::new("test");

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
        let err = subgraph_service
            .oneshot(SubgraphRequest {
                originating_request: Arc::new(
                    http_compat::Request::fake_builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Request::builder().query(Some("query".to_string())).build())
                        .build()
                        .expect("expecting valid request"),
                ),
                subgraph_request: http_compat::Request::fake_builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, "application/json")
                    .uri(url)
                    .body(Request::builder().query(Some("query".to_string())).build())
                    .build()
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "HTTP fetch failed from 'test': subgraph HTTP status error '400 Bad Request': BAD REQUEST)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_content_type() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2525").unwrap();
        tokio::task::spawn(emulate_subgraph_bad_response_format(socket_addr));
        let subgraph_service = SubgraphService::new("test");

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
        let err = subgraph_service
            .oneshot(SubgraphRequest {
                originating_request: Arc::new(
                    http_compat::Request::fake_builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Request::builder().query(Some("query".to_string())).build())
                        .build()
                        .expect("expecting valid request"),
                ),
                subgraph_request: http_compat::Request::fake_builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, "application/json")
                    .uri(url)
                    .body(Request::builder().query(Some("query".to_string())).build())
                    .build()
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "HTTP fetch failed from 'test': subgraph didn't return JSON (expected content-type: application/json or content-type: application/graphql+json; found content-type: \"text/html\")"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_compressed_request_response_body() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2727").unwrap();
        tokio::task::spawn(emulate_subgraph_compressed_response(socket_addr));
        let subgraph_service = SubgraphService::new("test");

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
        let resp = subgraph_service
            .oneshot(SubgraphRequest {
                originating_request: Arc::new(
                    http_compat::Request::fake_builder()
                        .header(HOST, "host")
                        .header(CONTENT_TYPE, "application/json")
                        .body(Request::builder().query(Some("query".to_string())).build())
                        .build()
                        .expect("expecting valid request"),
                ),
                subgraph_request: http_compat::Request::fake_builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, "application/json")
                    .header(CONTENT_ENCODING, "gzip")
                    .uri(url)
                    .body(Request::builder().query(Some("query".to_string())).build())
                    .build()
                    .expect("expecting valid request"),
                operation_kind: OperationKind::Query,
                context: Context::new(),
            })
            .await
            .unwrap();
        // Test the right decompression of the body
        let resp_from_subgraph = Response {
            data: Some(Value::String(ByteString::from("test"))),
            path: None,
            label: None,
            errors: vec![],
            extensions: Object::new(),
        };

        assert_eq!(resp.response.body(), &resp_from_subgraph);
    }
}
