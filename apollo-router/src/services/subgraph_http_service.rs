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
use http::HeaderMap;
use http::HeaderValue;
use hyper::client::HttpConnector;
use hyper_rustls::HttpsConnector;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use schemars::JsonSchema;
use tokio::io::AsyncWriteExt;
use tower::util::BoxService;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_http::decompression::Decompression;
use tower_http::decompression::DecompressionLayer;
use tracing::Instrument;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::layers::content_negociation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use super::Plugins;
use crate::error::FetchError;
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
pub(crate) struct SubgraphHTTPService {
    service_name: Arc<String>,
    // Note: We use hyper::Client here in preference to reqwest to avoid expensive URL translation
    // in the hot path. We use reqwest elsewhere because it's convenient and some of the
    // opentelemetry crate require reqwest clients to work correctly (at time of writing).
    client: Decompression<hyper::Client<HttpsConnector<HttpConnector>>>,
}

impl SubgraphHTTPService {
    pub(crate) fn new(service_name: impl Into<String>) -> Self {
        let mut http_connector = HttpConnector::new();
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http_connector);

        Self {
            client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(hyper::Client::builder().build(connector)),
            service_name: Arc::new(service_name.into()),
        }
    }
}

impl tower::Service<crate::SubgraphHTTPRequest> for SubgraphHTTPService {
    type Response = crate::SubgraphHTTPResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: crate::SubgraphHTTPRequest) -> Self::Future {
        let crate::SubgraphHTTPRequest {
            subgraph_request,
            context,
            ..
        } = request;

        let clone = self.client.clone();

        let mut client = std::mem::replace(&mut self.client, clone);

        let service_name = self.service_name.to_string();
        Box::pin(async move {
            let (parts, body) = subgraph_request.into_parts();

            let bytes = hyper::body::to_bytes(body).await?;

            let compressed_body = compress(bytes, &parts.headers)
                .instrument(tracing::debug_span!("body_compression"))
                .await
                .map_err(|err| {
                    tracing::error!(compress_error = format!("{:?}", err).as_str());

                    FetchError::CompressionError {
                        service: service_name.clone(),
                        reason: err.to_string(),
                    }
                })?;

            let mut request = http::request::Request::from_parts(parts, compressed_body.into());
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
            let response = client
                .call(request)
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

            let response = http::Response::from_parts(parts, hyper::Body::from(body));

            Ok(crate::SubgraphHTTPResponse { response, context })
        })
    }
}

pub(crate) async fn compress(body: Bytes, headers: &HeaderMap) -> Result<Bytes, BoxError> {
    let content_encoding = headers.get(&CONTENT_ENCODING);
    let as_vec = match content_encoding {
        Some(content_encoding) => match content_encoding.to_str()? {
            "br" => {
                let mut br_encoder = BrotliEncoder::new(Vec::new());
                br_encoder.write_all(&body).await?;
                br_encoder.shutdown().await?;

                br_encoder.into_inner()
            }
            "gzip" => {
                let mut gzip_encoder = GzipEncoder::new(Vec::new());
                gzip_encoder.write_all(&body).await?;
                gzip_encoder.shutdown().await?;

                gzip_encoder.into_inner()
            }
            "deflate" => {
                let mut df_encoder = ZlibEncoder::new(Vec::new());
                df_encoder.write_all(&body).await?;
                df_encoder.shutdown().await?;

                df_encoder.into_inner()
            }
            "identity" => body.to_vec(),
            unknown => {
                tracing::error!("unknown content-encoding value '{:?}'", unknown);
                Err(BoxError::from(format!(
                    "unknown content-encoding value '{:?}'",
                    unknown
                )))?
            }
        },
        None => body.to_vec(),
    };

    Ok(Bytes::from(as_vec))
}

pub(crate) trait SubgraphHTTPServiceFactory: Clone + Send + Sync + 'static {
    type SubgraphHTTPService: Service<
            crate::SubgraphHTTPRequest,
            Response = crate::SubgraphHTTPResponse,
            Error = BoxError,
            Future = Self::Future,
        > + Send
        + 'static;
    type Future: Send + 'static;

    fn create(&self, name: &str) -> Self::SubgraphHTTPService;
}

#[derive(Clone)]
pub(crate) struct SubgraphHTTPCreator {
    pub(crate) plugins: Arc<Plugins>,
}

impl SubgraphHTTPCreator {
    pub(crate) fn new(plugins: Arc<Plugins>) -> Self {
        SubgraphHTTPCreator { plugins }
    }
}

/// make new instances of the subgraph http service
///
/// there can be multiple instances of that service executing at any given time
pub(crate) trait MakeSubgraphHTTPService: Send + Sync + 'static {
    fn make(&self)
        -> BoxService<crate::SubgraphHTTPRequest, crate::SubgraphHTTPResponse, BoxError>;
}

impl<S> MakeSubgraphHTTPService for S
where
    S: Service<
            crate::SubgraphHTTPRequest,
            Response = crate::SubgraphHTTPResponse,
            Error = BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <S as Service<crate::SubgraphHTTPRequest>>::Future: Send,
{
    fn make(
        &self,
    ) -> BoxService<crate::SubgraphHTTPRequest, crate::SubgraphHTTPResponse, BoxError> {
        self.clone().boxed()
    }
}

impl SubgraphHTTPServiceFactory for SubgraphHTTPCreator {
    type SubgraphHTTPService =
        BoxService<crate::SubgraphHTTPRequest, crate::SubgraphHTTPResponse, BoxError>;
    type Future =
        <BoxService<crate::SubgraphHTTPRequest, crate::SubgraphHTTPResponse, BoxError> as Service<
            crate::SubgraphHTTPRequest,
        >>::Future;

    fn create(&self, name: &str) -> Self::SubgraphHTTPService {
        let service = SubgraphHTTPService::new(name).boxed();
        self.plugins
            .iter()
            .rev()
            .fold(service, |acc, (_, e)| e.subgraph_http_service(name, acc))
    }
}
