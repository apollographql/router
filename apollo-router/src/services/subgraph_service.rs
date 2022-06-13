//! Tower fetcher for subgraphs.

use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::{
    header::{self, ACCEPT, CONTENT_TYPE},
    HeaderValue, StatusCode,
};
use hyper::client::HttpConnector;
use hyper_rustls::HttpsConnector;
use opentelemetry::{global, trace::SpanKind};
use std::sync::Arc;
use std::task::Poll;
use tower::{BoxError, ServiceBuilder};
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub struct SubgraphService {
    client: hyper::Client<HttpsConnector<HttpConnector>>,
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
            client: ServiceBuilder::new().service(hyper::Client::builder().build(connector)),
            service: Arc::new(service.into()),
        }
    }
}

impl tower::Service<crate::SubgraphRequest> for SubgraphService {
    type Response = crate::SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: crate::SubgraphRequest) -> Self::Future {
        let crate::SubgraphRequest {
            subgraph_request,
            context,
            ..
        } = request;

        let mut client = self.client.clone();
        let service_name = (*self.service).to_owned();

        Box::pin(async move {
            let (parts, body) = subgraph_request.into_parts();

            let body = serde_json::to_string(&body).expect("JSON serialization should not fail");

            let mut request = http::request::Request::from_parts(parts, body.into());
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

                    crate::FetchError::SubrequestHttpError {
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
                        return Err(BoxError::from(crate::FetchError::SubrequestHttpError {
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

                    crate::FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;
            if parts.status != StatusCode::OK {
                return Err(BoxError::from(crate::FetchError::SubrequestHttpError {
                    service: service_name.clone(),
                    reason: format!(
                        "subgraph HTTP status error '{}': {})",
                        parts.status,
                        String::from_utf8_lossy(&body)
                    ),
                }));
            }

            let graphql: crate::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    crate::Response::from_bytes(&service_name, body).map_err(|error| {
                        crate::FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;

            let resp = http::Response::from_parts(parts, graphql);

            Ok(crate::SubgraphResponse::new_from_response(
                resp.into(),
                context,
            ))
        })
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
    use tower::{service_fn, ServiceExt};

    use crate::fetch::OperationKind;
    use crate::{http_compat, Context, Request, SubgraphRequest};

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
                        .body(Request::builder().query("query").build())
                        .build()
                        .expect("expecting valid request"),
                ),
                subgraph_request: http_compat::Request::fake_builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, "application/json")
                    .uri(url)
                    .body(Request::builder().query("query").build())
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
                        .body(Request::builder().query("query").build())
                        .build()
                        .expect("expecting valid request"),
                ),
                subgraph_request: http_compat::Request::fake_builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_TYPE, "application/json")
                    .uri(url)
                    .body(Request::builder().query("query").build())
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
}
