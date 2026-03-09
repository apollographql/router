use std::error::Error as _;
use std::fmt::Display;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use ::serde::Deserialize;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Request;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http_body_util::BodyExt;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
#[cfg(unix)]
use hyperlocal::UnixConnector;
use opentelemetry::global::get_text_map_propagator;
use opentelemetry_semantic_conventions::attribute::HTTP_RESPONSE_STATUS_CODE;
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
use tracing::Span;

use super::HttpRequest;
use super::HttpResponse;
use crate::Configuration;
use crate::axum_factory::compression::Compressor;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::plugins::authentication::subgraph::SigningParamsConfig;
use crate::plugins::telemetry::config_new::attributes::ERROR_TYPE;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::otel::OpenTelemetrySpanExt;
use crate::plugins::telemetry::reload::otel::prepare_context;
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
// TODO: make this configurable: ROUTER-1589
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

    pub(crate) fn from_config_for_coprocessor(
        tls_root_store: &RootCertStore,
        client_config: crate::configuration::shared::Client,
    ) -> Result<Self, BoxError> {
        // Coprocessors don't use client certificates, so use no client auth
        let tls_client_config = generate_tls_client_config(tls_root_store.clone(), None)?;

        HttpClientService::new("coprocessor".to_string(), tls_client_config, client_config)
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
                .pool_idle_timeout(None)
                .http2_only(http2 == Http2Config::Http2Only)
                .build(connector);

        #[cfg(unix)]
        let unix_client = {
            let unix_client_inner =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .pool_idle_timeout(None)
                    .http2_only(http2 == Http2Config::Http2Only)
                    .build(UnixConnector);

            ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(unix_client_inner)
        };

        Ok(Self {
            http_client: ServiceBuilder::new()
                .layer(DecompressionLayer::new())
                .service(http_client),
            #[cfg(unix)]
            unix_client,
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
        // WARN: we only check http_client, not unix_client, because we don't know which one will
        // be used and both the http client and the unix client use hyper_util's legacy client,
        // which is always ready (it queues internally); if that changes, we probably need to
        // update this to wait for both to be ready
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
        let http_req_span = Span::current();

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
    let span = tracing::Span::current();
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

    span.set_span_dyn_attribute(
        opentelemetry::Key::new(HTTP_RESPONSE_STATUS_CODE),
        opentelemetry::Value::I64(parts.status.as_u16() as i64),
    );

    if !parts.status.is_success() {
        span.set_span_dyn_attribute(
            opentelemetry::Key::new(ERROR_TYPE),
            opentelemetry::Value::String(parts.status.as_str().to_owned().into()),
        );
    }

    Ok(http::Response::from_parts(
        parts,
        RouterBody::new(body.map_err(axum::Error::new)),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::Mutex;

    use http::StatusCode;
    use http::Uri;
    use http::header::CONTENT_TYPE;
    use hyper_rustls::ConfigBuilderExt;
    use mime::APPLICATION_JSON;
    use tokio::net::TcpListener;
    use tower::ServiceExt;
    use tracing::Subscriber;
    use tracing::subscriber::DefaultGuard;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;

    use crate::Context;
    use crate::plugins::telemetry::dynamic_attribute::DynAttributeLayer;
    use crate::plugins::telemetry::otel;
    use crate::plugins::telemetry::otel::OtelData;
    use crate::services::http::BoxService;
    use crate::services::http::HttpClientService;
    use crate::services::http::HttpRequest;
    use crate::services::router;

    async fn emulate_subgraph_with_status_code(listener: TcpListener, status_code: StatusCode) {
        crate::services::http::tests::serve(listener, move |_| async move {
            Ok(http::Response::builder()
                .status(status_code)
                .body(r#"{}"#.into())
                .unwrap())
        })
        .await
        .unwrap();
    }

    async fn emulate_subgraph_with_header(
        listener: TcpListener,
        key: &'static str,
        value: &'static str,
    ) {
        crate::services::http::tests::serve(listener, move |_| async move {
            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .header(key, value)
                .body(r#"{}"#.into())
                .unwrap())
        })
        .await
        .unwrap();
    }

    #[derive(Default, Clone)]
    struct RecordingLayer {
        pub values: Arc<Mutex<HashMap<String, opentelemetry::Value>>>,
    }

    impl RecordingLayer {
        fn get(&self, key: &str) -> Option<opentelemetry::Value> {
            self.values.lock().unwrap().get(key).cloned()
        }
    }

    impl<S> Layer<S> for RecordingLayer
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        fn on_exit(
            &self,
            id: &tracing_core::span::Id,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut map = self.values.lock().unwrap();
            if let Some(span) = ctx.span(id)
                && let Some(otel_data) = span.extensions().get::<OtelData>()
                && let Some(attributes) = otel_data.builder.attributes.as_ref()
            {
                for attribute in attributes {
                    map.insert(attribute.key.to_string(), attribute.value.clone());
                }
            }
        }
    }

    fn setup_tracing() -> (DefaultGuard, RecordingLayer) {
        let recording_layer = RecordingLayer::default();
        let layer = DynAttributeLayer;
        let subscriber = tracing_subscriber::Registry::default()
            .with(layer)
            .with(otel::layer().force_sampling())
            .with(recording_layer.clone());
        let guard = tracing::subscriber::set_default(subscriber);
        (guard, recording_layer)
    }

    async fn make_telemetry_http_client(service_name: &str) -> BoxService {
        let full_config = serde_json::json!({
            "telemetry": {}
        });
        let telemetry_config = full_config
            .as_object()
            .expect("must be an object")
            .get("telemetry")
            .expect("telemetry must be a root key");
        let init = crate::plugin::PluginInit::fake_builder()
            .config(telemetry_config.clone())
            .full_config(full_config)
            .build()
            .with_deserialized_config()
            .expect("unable to deserialize telemetry config");
        let plugin = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(init)
            .await
            .expect("unable to create telemetry plugin");

        let http_client_service = HttpClientService::new(
            service_name,
            rustls::ClientConfig::builder()
                .with_native_roots()
                .expect("Able to load native roots")
                .with_no_client_auth(),
            crate::configuration::shared::Client::builder().build(),
        )
        .expect("can create a HttpClientService");

        plugin.http_client_service(service_name, BoxService::new(http_client_service))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_client_adds_status_code_and_error_type_attributes_to_500_span() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();

        tokio::task::spawn(emulate_subgraph_with_status_code(
            listener,
            StatusCode::INTERNAL_SERVER_ERROR,
        ));

        let telemetry_wrapped_service = make_telemetry_http_client("test").await;

        let (_guard, recording_layer) = setup_tracing();

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = telemetry_wrapped_service
            .oneshot(HttpRequest {
                http_request: http::Request::builder()
                    .uri(url)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(router::body::from_bytes(r#"{"query":"{ me { name } }"#))
                    .unwrap(),
                context: Context::new(),
            })
            .await
            .unwrap();

        assert_eq!(
            response.http_response.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        assert_eq!(
            recording_layer.get("http.response.status_code"),
            Some(opentelemetry::Value::I64(500))
        );
        assert_eq!(
            recording_layer.get("error.type"),
            Some(opentelemetry::Value::String("500".to_string().into())),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_client_adds_status_code_attributes_to_200_span() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();

        tokio::task::spawn(emulate_subgraph_with_status_code(listener, StatusCode::OK));

        let telemetry_wrapped_service = make_telemetry_http_client("test").await;

        let (_guard, recording_layer) = setup_tracing();

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = telemetry_wrapped_service
            .oneshot(HttpRequest {
                http_request: http::Request::builder()
                    .uri(url)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .body(router::body::from_bytes(r#"{"query":"{ me { name } }"#))
                    .unwrap(),
                context: Context::new(),
            })
            .await
            .unwrap();

        assert_eq!(response.http_response.status(), StatusCode::OK);

        assert_eq!(
            recording_layer.get("http.response.status_code"),
            Some(opentelemetry::Value::I64(200))
        );
        assert_eq!(recording_layer.get("error.type"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_client_span_attributes_from_config() {
        fn setup_tracing() -> (DefaultGuard, RecordingLayer) {
            let recording_layer = RecordingLayer::default();
            let layer = DynAttributeLayer;
            let subscriber = tracing_subscriber::Registry::default()
                .with(layer)
                .with(otel::layer().force_sampling())
                .with(recording_layer.clone());
            let guard = tracing::subscriber::set_default(subscriber);
            (guard, recording_layer)
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();

        tokio::task::spawn(emulate_subgraph_with_header(
            listener,
            "x-response-header",
            "response-value",
        ));

        let full_config = serde_json::json!({
            "telemetry": {
                "instrumentation": {
                    "spans": {
                        "http_client": {
                            "attributes": {
                                "custom_request_header": {
                                    "request_header": "x-request-header"
                                },
                                "custom_response_header": {
                                    "response_header": "x-response-header"
                                }
                            }
                        }
                    }
                }
            }
        });

        let telemetry_config = full_config
            .as_object()
            .expect("must be an object")
            .get("telemetry")
            .expect("telemetry must be a root key");
        let init = crate::plugin::PluginInit::fake_builder()
            .config(telemetry_config.clone())
            .full_config(full_config)
            .build()
            .with_deserialized_config()
            .expect("unable to deserialize telemetry config");

        let plugin = crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.telemetry")
            .expect("Plugin not found")
            .create_instance(init)
            .await
            .expect("unable to create telemetry plugin");

        // Create HTTP client service
        let http_client_service = HttpClientService::new(
            "test",
            rustls::ClientConfig::builder()
                .with_native_roots()
                .expect("Able to load native roots")
                .with_no_client_auth(),
            crate::configuration::shared::Client::builder().build(),
        )
        .expect("can create a HttpClientService");

        // Wrap with telemetry plugin
        let mut telemetry_wrapped_service =
            plugin.http_client_service("test", BoxService::new(http_client_service));

        let (_guard, recording_layer) = setup_tracing();

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = telemetry_wrapped_service
            .ready()
            .await
            .unwrap()
            .oneshot(HttpRequest {
                http_request: http::Request::builder()
                    .uri(url)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .header("x-request-header", "request-value")
                    .body(router::body::from_bytes(r#"{"query":"{ me { name } }"#))
                    .unwrap(),
                context: Context::new(),
            })
            .await
            .unwrap();

        assert_eq!(response.http_response.status(), StatusCode::OK);

        // Assert that the configured request header attribute is present
        assert_eq!(
            recording_layer.get("custom_request_header"),
            Some(opentelemetry::Value::String(
                "request-value".to_string().into()
            ))
        );

        // Assert that the configured response header attribute is present
        assert_eq!(
            recording_layer.get("custom_response_header"),
            Some(opentelemetry::Value::String(
                "response-value".to_string().into()
            ))
        );
    }
}
