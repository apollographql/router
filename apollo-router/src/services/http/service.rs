use std::fmt::Display;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use ::serde::Deserialize;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::Stream;
use futures::TryFutureExt;
use global::get_text_map_propagator;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::HeaderValue;
use http::Request;
use hyper::client::HttpConnector;
use hyper::Body;
use hyper_rustls::HttpsConnector;
#[cfg(unix)]
use hyperlocal::UnixConnector;
use opentelemetry::global;
use pin_project_lite::pin_project;
use rustls::ClientConfig;
use rustls::RootCertStore;
use schemars::JsonSchema;
use tower::util::Either;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower_http::decompression::Decompression;
use tower_http::decompression::DecompressionBody;
use tower_http::decompression::DecompressionLayer;
use tracing::Instrument;

use super::HttpRequest;
use super::HttpResponse;
use crate::axum_factory::compression::Compressor;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
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
#[cfg(unix)]
type UnixHTTPClient = Decompression<hyper::Client<UnixConnector, Body>>;
#[cfg(unix)]
type MixedClient = Either<HTTPClient, UnixHTTPClient>;
#[cfg(not(unix))]
type MixedClient = HTTPClient;

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
    http_client: HTTPClient,
    #[cfg(unix)]
    unix_client: UnixHTTPClient,
    service: Arc<String>,
}

impl HttpClientService {
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &Configuration,
        tls_root_store: &RootCertStore,
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
            .unwrap_or_else(|| tls_root_store.clone());
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
            http_client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(http_client),
            #[cfg(unix)]
            unix_client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(hyper::Client::builder().build(UnixConnector)),
            service: Arc::new(service.into()),
        })
    }

    pub(crate) fn native_roots_store() -> RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        let mut valid_count = 0;
        let mut invalid_count = 0;

        for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs")
        {
            let cert = rustls::Certificate(cert.0);
            match roots.add(&cert) {
                Ok(_) => valid_count += 1,
                Err(err) => {
                    tracing::trace!("invalid cert der {:?}", cert.0);
                    tracing::debug!("certificate parsing failed: {:?}", err);
                    invalid_count += 1
                }
            }
        }
        tracing::debug!(
            "with_native_roots processed {} valid and {} invalid certs",
            valid_count,
            invalid_count
        );
        assert!(!roots.is_empty(), "no CA certificates found");
        roots
    }
}

pub(crate) fn generate_tls_client_config(
    tls_cert_store: RootCertStore,
    client_cert_config: Option<&TlsClientAuth>,
) -> Result<rustls::ClientConfig, BoxError> {
    let tls_builder = rustls::ClientConfig::builder().with_safe_defaults();
    Ok(match client_cert_config {
        Some(client_auth_config) => tls_builder
            .with_root_certificates(tls_cert_store)
            .with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone(),
            )?,
        None => tls_builder
            .with_root_certificates(tls_cert_store)
            .with_no_client_auth(),
    })
}

impl tower::Service<HttpRequest> for HttpClientService {
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http_client
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

        #[cfg(unix)]
        let client = match schema_uri.scheme().map(|s| s.as_str()) {
            Some("unix") => Either::B(self.unix_client.clone()),
            _ => Either::A(self.http_client.clone()),
        };
        #[cfg(not(unix))]
        let client = self.http_client.clone();

        let service_name = self.service.clone();

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

        let (parts, body) = http_request.into_parts();

        let content_encoding = parts.headers.get(&CONTENT_ENCODING);
        let opt_compressor = content_encoding
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .and_then(|v| Compressor::new(v.split(',').map(|s| s.trim())));

        let body = match opt_compressor {
            None => body,
            Some(compressor) => Body::wrap_stream(compressor.process(body)),
        };
        let mut http_request = http::Request::from_parts(parts, body);

        http_request
            .headers_mut()
            .insert(ACCEPT_ENCODING, ACCEPTED_ENCODINGS.clone());

        let signing_params = context
            .extensions()
            .lock()
            .get::<SigningParamsConfig>()
            .cloned();

        Box::pin(async move {
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
    mut client: MixedClient,
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

pin_project! {
    pub(crate) struct BodyStream<B: hyper::body::HttpBody> {
        #[pin]
        inner: DecompressionBody<B>
    }
}

impl<B: hyper::body::HttpBody> BodyStream<B> {
    /// Create a new `BodyStream`.
    pub(crate) fn new(body: DecompressionBody<B>) -> Self {
        Self { inner: body }
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
