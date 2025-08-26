use std::error::Error as _;
use std::fmt::Display;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use ::serde::Deserialize;
use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::HeaderValue;
use http::Request;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http_body_util::BodyExt;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
#[cfg(unix)]
use hyperlocal::UnixConnector;
use opentelemetry::global;
use rustls::ClientConfig;
use rustls::RootCertStore;
use schemars::JsonSchema;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
#[cfg(unix)]
use tower::util::Either;
use tower_http::decompression::Decompression;
use tower_http::decompression::DecompressionLayer;
use tracing::Instrument;

use super::HttpRequest;
use super::HttpResponse;
use crate::Configuration;
use crate::axum_factory::compression::Compressor;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::telemetry::consts::HTTP_REQUEST_SPAN_NAME;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::prepare_context;
use crate::plugins::traffic_shaping::Http2Config;
use crate::services::hickory_dns_connector::AsyncHyperResolver;
use crate::services::hickory_dns_connector::new_async_http_connector;
use crate::services::router;
use crate::services::router::body::RouterBody;

type HTTPClient = Decompression<
    hyper_util::client::legacy::Client<
        HttpsConnector<HttpConnector<AsyncHyperResolver>>,
        RouterBody,
    >,
>;
#[cfg(unix)]
type UnixHTTPClient = Decompression<hyper_util::client::legacy::Client<UnixConnector, RouterBody>>;
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
    // Note: We use hyper_util::client::legacy::Client here in preference to reqwest to avoid expensive URL translation
    // in the hot path. We use reqwest elsewhere because it's convenient and some of the
    // opentelemetry crate require reqwest clients to work correctly (at time of writing).
    http_client: HTTPClient,
    #[cfg(unix)]
    unix_client: UnixHTTPClient,
    service: Arc<String>,
}

impl HttpClientService {
    pub(crate) fn from_config_for_subgraph(
        service: impl Into<String>,
        configuration: &Configuration,
        tls_root_store: &RootCertStore,
        client_config: crate::configuration::shared::Client,
    ) -> Result<Self, BoxError> {
        let name: String = service.into();
        let default_client_cert_config = configuration
            .tls
            .subgraph
            .all
            .client_authentication
            .as_ref();

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
            .or(default_client_cert_config);

        let tls_client_config =
            generate_tls_client_config(tls_cert_store, client_cert_config.map(|arc| arc.as_ref()))?;

        HttpClientService::new(name, tls_client_config, client_config)
    }

    pub(crate) fn from_config_for_connector(
        source_name: impl Into<String>,
        configuration: &Configuration,
        tls_root_store: &RootCertStore,
        client_config: crate::configuration::shared::Client,
    ) -> Result<Self, BoxError> {
        let name: String = source_name.into();
        let default_client_cert_config = configuration
            .tls
            .connector
            .all
            .client_authentication
            .as_ref();

        let tls_cert_store = configuration
            .tls
            .connector
            .sources
            .get(&name)
            .as_ref()
            .and_then(|subgraph| subgraph.create_certificate_store())
            .transpose()?
            .unwrap_or_else(|| tls_root_store.clone());
        let client_cert_config = configuration
            .tls
            .connector
            .sources
            .get(&name)
            .as_ref()
            .and_then(|tls| tls.client_authentication.as_ref())
            .or(default_client_cert_config);

        let tls_client_config =
            generate_tls_client_config(tls_cert_store, client_cert_config.map(|arc| arc.as_ref()))?;

        HttpClientService::new(name, tls_client_config, client_config)
    }

    pub(crate) fn new(
        service: impl Into<String>,
        tls_config: ClientConfig,
        client_config: crate::configuration::shared::Client,
    ) -> Result<Self, BoxError> {
        let mut http_connector =
            new_async_http_connector(client_config.dns_resolution_strategy.unwrap_or_default())?;
        http_connector.set_nodelay(true);
        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
        http_connector.enforce_http(false);

        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1();

        let http2 = client_config.experimental_http2.unwrap_or_default();
        let connector = if http2 != Http2Config::Disable {
            builder.enable_http2().wrap_connector(http_connector)
        } else {
            builder.wrap_connector(http_connector)
        };

        let http_client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
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
                .service(
                    hyper_util::client::legacy::Client::builder(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .build(UnixConnector),
                ),
            service: Arc::new(service.into()),
        })
    }

    pub(crate) fn native_roots_store() -> RootCertStore {
        let mut roots = rustls::RootCertStore::empty();

        roots.add_parsable_certificates(
            rustls_native_certs::load_native_certs().expect("could not load platform certs"),
        );

        assert!(!roots.is_empty(), "no CA certificates found");
        roots
    }
}

pub(crate) fn generate_tls_client_config(
    tls_cert_store: RootCertStore,
    client_cert_config: Option<&TlsClientAuth>,
) -> Result<rustls::ClientConfig, BoxError> {
    let tls_builder = rustls::ClientConfig::builder();

    Ok(match client_cert_config {
        Some(client_auth_config) => tls_builder
            .with_root_certificates(tls_cert_store)
            .with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone_key(),
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
            ..
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
            Some("unix") => {
                // Because we clone our inner service, we'd better swap the readied one
                let clone = self.unix_client.clone();
                Either::Right(std::mem::replace(&mut self.unix_client, clone))
            }
            _ => {
                // Because we clone our inner service, we'd better swap the readied one
                let clone = self.http_client.clone();
                Either::Left(std::mem::replace(&mut self.http_client, clone))
            }
        };
        #[cfg(not(unix))]
        let client = {
            // Because we clone our inner service, we'd better swap the readied one
            let clone = self.http_client.clone();
            std::mem::replace(&mut self.http_client, clone)
        };

        let service_name = self.service.clone();

        let path = schema_uri.path();

        let http_req_span = tracing::info_span!(HTTP_REQUEST_SPAN_NAME,
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
                &mut crate::otel_compat::HeaderInjector(http_request.headers_mut()),
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
            Some(compressor) => router::body::from_result_stream(compressor.process(body)),
        };

        let mut http_request = http::Request::from_parts(parts, body);

        http_request
            .headers_mut()
            .insert(ACCEPT_ENCODING, ACCEPTED_ENCODINGS.clone());

        let signing_params = context
            .extensions()
            .with_lock(|lock| lock.get::<Arc<SigningParamsConfig>>().cloned());

        Box::pin(async move {
            let http_request = if let Some(signing_params) = signing_params {
                signing_params.sign(http_request, &service_name).await?
            } else {
                http_request
            };

            let http_response = do_fetch(client, &service_name, http_request)
                .instrument(http_req_span)
                .await?;

            Ok(HttpResponse {
                http_response,
                context,
            })
        })
    }
}

/// Hyper client errors are very opaque. This function peels back the layers and attempts to
/// provide a useful message to end users.
fn report_hyper_client_error(err: hyper_util::client::legacy::Error) -> String {
    // At the time of writing, a hyper-util error only prints "client error", and no useful further
    // information. So if we have a source error (always true in practice), we simply discard the
    // "client error" part and only report the inner error.
    let Some(source) = err.source() else {
        // No further information
        return err.to_string();
    };

    // If there was a connection, parsing, http, etc, error, the source will be a
    // `hyper::Error`. `hyper::Error` provides a minimal error message only, that
    // will explain vaguely where the problem is, like "error in user's Body stream",
    // or "error parsing http header".
    // This is important to preserve as it may clarify the difference between a malfunctioning
    // subgraph and a buggy router.
    // It's not enough information though, in particular for the user error kinds, so if there is
    // another inner error, we report *both* the hyper error and the inner error.
    let subsource = source
        .downcast_ref::<hyper::Error>()
        .and_then(|err| err.source());
    match subsource {
        Some(inner_err) => format!("{source}: {inner_err}"),
        None => source.to_string(),
    }
}

async fn do_fetch(
    mut client: MixedClient,
    service_name: &str,
    request: Request<RouterBody>,
) -> Result<http::Response<RouterBody>, FetchError> {
    let (parts, body) = client
        .call(request)
        .await
        .map_err(|err| {
            tracing::error!(fetch_error = ?err);
            FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.to_string(),
                reason: report_hyper_client_error(err),
            }
        })?
        .into_parts();
    Ok(http::Response::from_parts(
        parts,
        RouterBody::new(body.map_err(axum::Error::new)),
    ))
}
