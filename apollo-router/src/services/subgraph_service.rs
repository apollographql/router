//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use std::task::Poll;

use ::serde::Deserialize;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZlibEncoder;
use bytes::Bytes;
use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::header::ACCEPT;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::{self};
use http::HeaderMap;
use http::HeaderValue;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use schemars::JsonSchema;
use tokio::io::AsyncWriteExt;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::layers::content_negociation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use super::subgraph_http;
use super::subgraph_http_service::MakeSubgraphHTTPService;
use super::Plugins;
use crate::error::FetchError;
use crate::graphql;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Compression {
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
pub(crate) struct SubgraphService<SubgraphHTTPCreator>
where
    SubgraphHTTPCreator: Service<subgraph_http::Request, Response = subgraph_http::Response, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <SubgraphHTTPCreator as Service<subgraph_http::Request>>::Future: std::marker::Send,
{
    http_creator: SubgraphHTTPCreator, //Decompression<hyper::Client<HttpsConnector<HttpConnector>>>,
    service_name: Arc<String>,
}

impl<SubgraphHTTPCreator> SubgraphService<SubgraphHTTPCreator>
where
    SubgraphHTTPCreator: Service<subgraph_http::Request, Response = subgraph_http::Response, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <SubgraphHTTPCreator as Service<subgraph_http::Request>>::Future: std::marker::Send,
{
    pub(crate) fn new(service_name: impl Into<String>, http_creator: SubgraphHTTPCreator) -> Self {
        Self {
            http_creator,
            service_name: Arc::new(service_name.into()),
        }
    }
}

impl<SubgraphHTTPCreator> tower::Service<crate::SubgraphRequest>
    for SubgraphService<SubgraphHTTPCreator>
where
    SubgraphHTTPCreator: Service<subgraph_http::Request, Response = subgraph_http::Response, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <SubgraphHTTPCreator as Service<subgraph_http::Request>>::Future: std::marker::Send,
{
    type Response = crate::SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: crate::SubgraphRequest) -> Self::Future {
        let crate::SubgraphRequest {
            subgraph_request,
            context,
            ..
        } = request;

        let mut client = self.http_creator.make();
        let service_name = (*self.service_name).to_owned();

        Box::pin(async move {
            let (parts, body) = subgraph_request.into_parts();
            let body = serde_json::to_vec(&body).expect("JSON serialization should not fail");
            let mut request = http::request::Request::from_parts(parts, Bytes::from(body));

            let app_json: HeaderValue = HeaderValue::from_static(APPLICATION_JSON.essence_str());
            let app_graphql_json: HeaderValue =
                HeaderValue::from_static(GRAPHQL_JSON_RESPONSE_HEADER_VALUE);
            request.headers_mut().insert(CONTENT_TYPE, app_json.clone());
            request.headers_mut().insert(ACCEPT, app_json);
            request.headers_mut().append(ACCEPT, app_graphql_json);

            get_text_map_propagator(|propagator| {
                propagator.inject_context(
                    &Span::current().context(),
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                );
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
            let display_headers = context.contains_key(LOGGING_DISPLAY_HEADERS);
            let display_body = context.contains_key(LOGGING_DISPLAY_BODY);
            if display_headers {
                tracing::info!(http.request.headers = ?request.headers(), apollo.subgraph.name = %service_name, "Request headers to subgraph {service_name:?}");
            }
            if display_body {
                tracing::info!(http.request.body = ?request.body(), apollo.subgraph.name = %service_name, "Request body to subgraph {service_name:?}");
            }

            let path = schema_uri.path().to_string();

            let subgraph_http_request = subgraph_http::Request {
                subgraph_request: request,
                context: context.clone(),
            };

            let subgraph_http::Response { response, context } = client
                .call(subgraph_http_request)
                .instrument(tracing::info_span!("subgraph_request",
                    "otel.kind" = "CLIENT",
                    "net.peer.name" = &display(host),
                    "net.peer.port" = &display(port),
                    "http.route" = &display(path),
                    "net.transport" = "ip_tcp",
                    "apollo.subgraph.name" = %service_name
                ))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            // Keep our parts, we'll need them later
            let (parts, body) = response.into_parts();
            if display_headers {
                tracing::info!(
                    http.response.headers = ?parts.headers, apollo.subgraph.name = %service_name, "Response headers from subgraph {service_name:?}"
                );
            }
            if let Some(content_type) = parts.headers.get(header::CONTENT_TYPE) {
                if let Ok(content_type_str) = content_type.to_str() {
                    // Using .contains because sometimes we could have charset included (example: "application/json; charset=utf-8")
                    if !content_type_str.contains(APPLICATION_JSON.essence_str())
                        && !content_type_str.contains(GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                    {
                        return if !parts.status.is_success() {
                            Err(BoxError::from(FetchError::SubrequestHttpError {
                                service: service_name.clone(),
                                reason: format!(
                                    "{}: {}",
                                    parts.status.as_str(),
                                    parts.status.canonical_reason().unwrap_or("Unknown")
                                ),
                            }))
                        } else {
                            Err(BoxError::from(FetchError::SubrequestHttpError {
                                service: service_name.clone(),
                                reason: format!("subgraph didn't return JSON (expected content-type: {} or content-type: {GRAPHQL_JSON_RESPONSE_HEADER_VALUE}; found content-type: {content_type:?})", APPLICATION_JSON.essence_str()),
                            }))
                        };
                    }
                }
            }

            let body = hyper::body::to_bytes(body)
                .instrument(tracing::debug_span!("aggregate_response_data"))
                .await
                .map_err(|err| {
                    tracing::error!(fetch_error = format!("{:?}", err).as_str());

                    FetchError::SubrequestHttpError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            if display_body {
                tracing::info!(
                    http.response.body = %String::from_utf8_lossy(&body), apollo.subgraph.name = %service_name, "Raw response body from subgraph {service_name:?} received"
                );
            }

            let graphql: graphql::Response = tracing::debug_span!("parse_subgraph_response")
                .in_scope(|| {
                    graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                        FetchError::SubrequestMalformedResponse {
                            service: service_name.clone(),
                            reason: error.to_string(),
                        }
                    })
                })?;

            let resp = http::Response::from_parts(parts, graphql);

            Ok(crate::SubgraphResponse::new_from_response(resp, context))
        })
    }
}

pub(crate) async fn compress(body: String, headers: &HeaderMap) -> Result<Vec<u8>, BoxError> {
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

pub(crate) trait SubgraphServiceFactory: Clone + Send + Sync + 'static {
    type SubgraphService: Service<
            crate::SubgraphRequest,
            Response = crate::SubgraphResponse,
            Error = BoxError,
            Future = Self::Future,
        > + Send
        + 'static;
    type Future: Send + 'static;

    fn create(&self, name: &str) -> Option<Self::SubgraphService>;
}

#[derive(Clone)]
pub(crate) struct SubgraphCreator {
    pub(crate) services: Arc<HashMap<String, Arc<dyn MakeSubgraphService>>>,

    pub(crate) plugins: Arc<Plugins>,
}

impl SubgraphCreator {
    pub(crate) fn new(
        services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
        plugins: Arc<Plugins>,
    ) -> Self {
        SubgraphCreator {
            services: Arc::new(services.into_iter().collect()),
            plugins,
        }
    }
}

/// make new instances of the subgraph service
///
/// there can be multiple instances of that service executing at any given time
pub(crate) trait MakeSubgraphService: Send + Sync + 'static {
    fn make(&self) -> BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>;
}

impl<S> MakeSubgraphService for S
where
    S: Service<crate::SubgraphRequest, Response = crate::SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <S as Service<crate::SubgraphRequest>>::Future: Send,
{
    fn make(&self) -> BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError> {
        self.clone().boxed()
    }
}

impl SubgraphServiceFactory for SubgraphCreator {
    type SubgraphService = BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError>;
    type Future =
        <BoxService<crate::SubgraphRequest, crate::SubgraphResponse, BoxError> as Service<
            crate::SubgraphRequest,
        >>::Future;

    fn create(&self, name: &str) -> Option<Self::SubgraphService> {
        self.services.get(name).map(|service| {
            let service = service.make();
            self.plugins
                .iter()
                .rev()
                .fold(service, |acc, (_, e)| e.subgraph_service(name, acc))
        })
    }
}
