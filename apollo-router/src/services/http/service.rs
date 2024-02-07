use std::fmt::Display;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use ::serde::Deserialize;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZlibEncoder;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::Stream;
use futures::TryFutureExt;
use global::get_text_map_propagator;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::HeaderMap;
use http::HeaderValue;
use http::Request;
use hyper::client::HttpConnector;
use hyper::Body;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use opentelemetry::global;
use pin_project_lite::pin_project;
use rustls::ClientConfig;
use rustls::RootCertStore;
use schemars::JsonSchema;
use tokio::io::AsyncWriteExt;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower_http::decompression::Decompression;
use tower_http::decompression::DecompressionBody;
use tower_http::decompression::DecompressionLayer;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::HttpRequest;
use super::HttpResponse;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::telemetry::reload::prepare_context;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;
use crate::plugins::traffic_shaping::Http2Config;
use crate::services::trust_dns_connector::new_async_http_connector;
use crate::services::trust_dns_connector::AsyncHyperResolver;
use crate::Configuration;
use crate::Context;

type HTTPClient =
    Decompression<hyper::Client<HttpsConnector<HttpConnector<AsyncHyperResolver>>, Body>>;

// interior mutability is not a concern here, the value is never modified
#[allow(clippy::declare_interior_mutable_const)]
static ACCEPTED_ENCODINGS: HeaderValue = HeaderValue::from_static("gzip, br, deflate");
const POOL_IDLE_TIMEOUT_DURATION: Option<Duration> = Some(Duration::from_secs(5));

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Compression {
    /// gzip
    Gzip,
    /// deflate
    Deflate,
    /// brotli
    Br,
    /// identity
    Identity,
}

impl Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compression::Gzip => write!(f, "gzip"),
            Compression::Deflate => write!(f, "deflate"),
            Compression::Br => write!(f, "br"),
            Compression::Identity => write!(f, "identity"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct HttpClientService {
    // Note: We use hyper::Client here in preference to reqwest to avoid expensive URL translation
    // in the hot path. We use reqwest elsewhere because it's convenient and some of the
    // opentelemetry crate require reqwest clients to work correctly (at time of writing).
    client: HTTPClient,
    service: Arc<String>,
}

impl HttpClientService {
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &Configuration,
        tls_root_store: &Option<RootCertStore>,
        http2: Http2Config,
    ) -> Result<Self, BoxError> {
        let name: String = service.into();
        let tls_cert_store = configuration
            .tls
            .subgraph
            .subgraphs
            .get(&name)
            .as_ref()
            .and_then(|subgraph| subgraph.create_certificate_store())
            .transpose()?
            .or_else(|| tls_root_store.clone());
        let client_cert_config = configuration
            .tls
            .subgraph
            .subgraphs
            .get(&name)
            .as_ref()
            .and_then(|tls| tls.client_authentication.as_ref())
            .or(configuration
                .tls
                .subgraph
                .all
                .client_authentication
                .as_ref());

        let tls_client_config = generate_tls_client_config(tls_cert_store, client_cert_config)?;

        HttpClientService::new(name, http2, tls_client_config)
    }

    pub(crate) fn new(
        service: impl Into<String>,
        http2: Http2Config,
        tls_config: ClientConfig,
    ) -> Result<Self, BoxError> {
        let mut http_connector = new_async_http_connector()?;
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1();

        let connector = if http2 != Http2Config::Disable {
            builder.enable_http2().wrap_connector(http_connector)
        } else {
            builder.wrap_connector(http_connector)
        };

        let http_client = hyper::Client::builder()
            .pool_idle_timeout(POOL_IDLE_TIMEOUT_DURATION)
            .http2_only(http2 == Http2Config::Http2Only)
            .build(connector);
        Ok(Self {
            client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(http_client),
            service: Arc::new(service.into()),
        })
    }
}

pub(crate) fn generate_tls_client_config(
    tls_cert_store: Option<RootCertStore>,
    client_cert_config: Option<&TlsClientAuth>,
) -> Result<rustls::ClientConfig, BoxError> {
    let tls_builder = rustls::ClientConfig::builder().with_safe_defaults();
    Ok(match (tls_cert_store, client_cert_config) {
        (None, None) => tls_builder.with_native_roots().with_no_client_auth(),
        (Some(store), None) => tls_builder
            .with_root_certificates(store)
            .with_no_client_auth(),
        (None, Some(client_auth_config)) => tls_builder.with_native_roots().with_client_auth_cert(
            client_auth_config.certificate_chain.clone(),
            client_auth_config.key.clone(),
        )?,
        (Some(store), Some(client_auth_config)) => tls_builder
            .with_root_certificates(store)
            .with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone(),
            )?,
    })
}

impl tower::Service<HttpRequest> for HttpClientService {
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map(|res| res.map_err(|e| Box::new(e) as BoxError))
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        let HttpRequest {
            mut http_request,
            context,
        } = request;

        let schema_uri = http_request.uri();
        let host = schema_uri.host().unwrap_or_default();
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

        let path = schema_uri.path();

        let http_req_span = tracing::info_span!("http_request",
            "otel.kind" = "CLIENT",
            "net.peer.name" = %host,
            "net.peer.port" = %port,
            "http.route" = %path,
            "http.url" = %schema_uri,
            "net.transport" = "ip_tcp",
            //"apollo.subgraph.name" = %service_name,
            //"graphql.operation.name" = %operation_name,
        );
        get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &prepare_context(http_req_span.context()),
                &mut opentelemetry_http::HeaderInjector(http_request.headers_mut()),
            );
        });

        let client = self.client.clone();
        let service_name = self.service.clone();
        Box::pin(async move {
            let (parts, body) = http_request.into_parts();
            let body = hyper::body::to_bytes(body).await.map_err(|err| {
                tracing::error!(compress_error = format!("{err:?}").as_str());

                FetchError::CompressionError {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                }
            })?;
            let compressed_body = compress(body, &parts.headers)
                .instrument(tracing::debug_span!("body_compression"))
                .await
                .map_err(|err| {
                    tracing::error!(compress_error = format!("{err:?}").as_str());

                    FetchError::CompressionError {
                        service: service_name.to_string(),
                        reason: err.to_string(),
                    }
                })?;
            let mut http_request = http::Request::from_parts(parts, Body::from(compressed_body));

            http_request
                .headers_mut()
                .insert(ACCEPT_ENCODING, ACCEPTED_ENCODINGS.clone());

            let signing_params = context
                .extensions()
                .lock()
                .get::<SigningParamsConfig>()
                .cloned();

            let http_request = if let Some(signing_params) = signing_params {
                signing_params.sign(http_request, &service_name).await?
            } else {
                http_request
            };

            let display_headers = context.contains_key(LOGGING_DISPLAY_HEADERS);
            let display_body = context.contains_key(LOGGING_DISPLAY_BODY);

            // Print out the debug for the request
            if display_headers {
                tracing::info!(http.request.headers = ?http_request.headers(), apollo.subgraph.name = %service_name, "Request headers to subgraph {service_name:?}");
            }
            if display_body {
                tracing::info!(http.request.body = ?http_request.body(), apollo.subgraph.name = %service_name, "Request body to subgraph {service_name:?}");
            }

            let http_response = do_fetch(client, &context, &service_name, http_request)
                .instrument(http_req_span)
                .await?;

            // Print out the debug for the response
            if display_headers {
                tracing::info!(response.headers = ?http_response.headers(), apollo.subgraph.name = %service_name, "Response headers from subgraph {service_name:?}");
            }

            Ok(HttpResponse {
                http_response,
                context,
            })
        })
    }
}

async fn do_fetch(
    mut client: HTTPClient,
    context: &Context,
    service_name: &str,
    request: Request<Body>,
) -> Result<http::Response<Body>, FetchError> {
    let _active_request_guard = context.enter_active_request();
    let (parts, body) = client
        .call(request)
        .map_err(|err| {
            tracing::error!(fetch_error = ?err);
            FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.to_string(),
                reason: err.to_string(),
            }
        })
        .await?
        .into_parts();
    Ok(http::Response::from_parts(
        parts,
        Body::wrap_stream(BodyStream { inner: body }),
    ))
}

pub(crate) async fn compress(body: Bytes, headers: &HeaderMap) -> Result<Bytes, BoxError> {
    let content_encoding = headers.get(&CONTENT_ENCODING);
    match content_encoding {
        Some(content_encoding) => match content_encoding.to_str()? {
            "br" => {
                let mut br_encoder = BrotliEncoder::new(Vec::new());
                br_encoder.write_all(&body).await?;
                br_encoder.shutdown().await?;

                Ok(br_encoder.into_inner().into())
            }
            "gzip" => {
                let mut gzip_encoder = GzipEncoder::new(Vec::new());
                gzip_encoder.write_all(&body).await?;
                gzip_encoder.shutdown().await?;

                Ok(gzip_encoder.into_inner().into())
            }
            "deflate" => {
                let mut df_encoder = ZlibEncoder::new(Vec::new());
                df_encoder.write_all(&body).await?;
                df_encoder.shutdown().await?;

                Ok(df_encoder.into_inner().into())
            }
            "identity" => Ok(body),
            unknown => {
                tracing::error!("unknown content-encoding value '{:?}'", unknown);
                Err(BoxError::from(format!(
                    "unknown content-encoding value '{unknown:?}'",
                )))
            }
        },
        None => Ok(body),
    }
}

pin_project! {
    pub(crate) struct BodyStream<B: hyper::body::HttpBody> {
        #[pin]
        inner: DecompressionBody<B>
    }
}

impl<B> Stream for BodyStream<B>
where
    B: hyper::body::HttpBody,
    B::Error: Into<tower_http::BoxError>,
{
    type Item = Result<Bytes, BoxError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        use hyper::body::HttpBody;

        self.project().inner.poll_data(cx)
    }
}
