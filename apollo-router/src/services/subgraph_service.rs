//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::task::Poll;

use ::serde::Deserialize;
use async_compression::tokio::write::BrotliEncoder;
use async_compression::tokio::write::GzipEncoder;
use async_compression::tokio::write::ZlibEncoder;
use futures::future::BoxFuture;
use global::get_text_map_propagator;
use http::header::ACCEPT;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::header::{self};
use http::HeaderMap;
use http::HeaderValue;
use hyper::client::HttpConnector;
use hyper::Client;
use hyper_rustls::HttpsConnector;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use schemars::JsonSchema;
use serde_json_bytes::{ByteString, Value};
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
use crate::services::layers::apq;
use crate::{graphql, Context};

const PERSISTED_QUERY_NOT_FOUND_ERR_STRING: &str = "PERSISTED_QUERY_NOT_FOUND";
const PERSISTED_QUERY_NOT_SUPPORTED_ERR_STRING: &str = "PERSISTED_QUERY_NOT_SUPPORTED";
const CODE_STRING: &str = "code";
const PERSISTED_QUERY_KEY: &str = "persistedQuery";
const HASH_VERSION_KEY: &str = "version";
const HASH_VERSION_VALUE: i32 = 1;
const HASH_KEY: &str = "sha256Hash";

enum APQError {
    PersistedQueryNotSupported,
    PersistedQueryNotFound,
    Other,
}

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
pub(crate) struct SubgraphService {
    // Note: We use hyper::Client here in preference to reqwest to avoid expensive URL translation
    // in the hot path. We use reqwest elsewhere because it's convenient and some of the
    // opentelemetry crate require reqwest clients to work correctly (at time of writing).
    client: Decompression<hyper::Client<HttpsConnector<HttpConnector>>>,
    service: Arc<String>,

    /// Whether apq is enabled in the router for subgraph calls
    /// This is enabled by default can be configured as
    /// subgraph:
    ///      apq_enabled: <bool>
    /// If a subgraph sends the error message PERSISTED_QUERY_NOT_SUPPORTED,
    /// apq_enabled is set to false
    apq_enabled: Arc<AtomicBool>,
}

impl SubgraphService {
    pub(crate) fn new(service: impl Into<String>, apq_enabled: bool) -> Self {
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
            service: Arc::new(service.into()),
            apq_enabled:  Arc::new(<AtomicBool>::new(apq_enabled)),
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
        } = request.clone();

        let (_, body) = subgraph_request.into_parts();

        let clone = self.client.clone();
        let client = std::mem::replace(&mut self.client, clone);
        let service_name = (*self.service).to_owned();

        let arc_apq_enabled = self.apq_enabled.clone();

        let make_calls = async move {
            // If APQ is not enabled, simply make the graphql call
            // with the same request body.
            let apq_enabled = arc_apq_enabled.as_ref();
            if !apq_enabled.load(Relaxed) {
                return call_http(request, body, context, client, service_name).await;
            }

            // Else, if apq is enabled,
            // Calculate the hash for query and try the request with
            // a persistedQuery instead of the whole query.
            let graphql::Request {
                query,
                operation_name,
                variables,
                extensions,
            } = body.clone();

            let hash_value = apq::calculate_hash_for_query(query.clone().unwrap_or_default());

            let persisted_query = serde_json_bytes::json!({
                HASH_VERSION_KEY: HASH_VERSION_VALUE,
                HASH_KEY: hash_value
            });

            let mut extensions_with_apq = extensions.clone();
            extensions_with_apq.insert(PERSISTED_QUERY_KEY, persisted_query);

            let mut apq_body = graphql::Request {
                query: None,
                operation_name: operation_name,
                variables: variables,
                extensions: extensions_with_apq,
            };

            let response = call_http(
                request.clone(),
                apq_body.clone(),
                context.clone(),
                client.clone(),
                service_name.clone(),
            )
            .await;

            // Check the error for the request with only persistedQuery.
            // If PersistedQueryNotSupported, stop trying apq for this subgraph service
            // If PersistedQueryNotFound, add the whole query to the request and retry.
            // Else, return the response like before.
            let http_response = response.as_ref();

            // http_response with APQ error returns a 200 OK http response.
            // The errors are contained in graphql Response.
            if http_response.is_err() {
                return response
            }

            let gql_resp = http_response.unwrap().response.body().clone();
            match get_apq_error(gql_resp) {
                APQError::PersistedQueryNotSupported => {
                    apq_enabled.store(false, Relaxed);
                    return call_http(request, body, context, client, service_name).await
                }
                APQError::PersistedQueryNotFound => {
                    apq_body.query = query;
                    return call_http(request, apq_body, context, client, service_name).await
                }
                _ => return response
            }
        };

        Box::pin(make_calls)
    }
}

/// call_http makes http calls with modified graphql::Request (body)
/// with or without the query and hash acc. to APQ need.
async fn call_http(
    request: crate::SubgraphRequest,
    body: graphql::Request,
    context: Context,
    mut client: Decompression<Client<HttpsConnector<HttpConnector>>>,
    service_name: String,
) -> Result<crate::SubgraphResponse, BoxError> {
    let crate::SubgraphRequest {
        subgraph_request, ..
    } = request;

    let (parts, _) = subgraph_request.into_parts();

    let body = serde_json::to_string(&body).expect("JSON serialization should not fail");
    let compressed_body = compress(body, &parts.headers)
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

    let schema_uri = request.uri().clone();
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

    let graphql: graphql::Response =
        tracing::debug_span!("parse_subgraph_response").in_scope(|| {
            graphql::Response::from_bytes(&service_name, body).map_err(|error| {
                FetchError::SubrequestMalformedResponse {
                    service: service_name.clone(),
                    reason: error.to_string(),
                }
            })
        })?;

    let resp = http::Response::from_parts(parts, graphql);

    Ok(crate::SubgraphResponse::new_from_response(resp, context))
}

fn get_apq_error(response: graphql::Response) -> APQError {
    let not_found_byte_string = &Value::String(ByteString::from(PERSISTED_QUERY_NOT_FOUND_ERR_STRING));
    let not_supported_byte_string = &Value::String(ByteString::from(PERSISTED_QUERY_NOT_SUPPORTED_ERR_STRING));
    for error in response.errors {
        match error.extensions.get(&ByteString::from(CODE_STRING)) {
            Some(value) => {
                if value == not_found_byte_string {
                    return APQError::PersistedQueryNotFound;
                } else if value == not_supported_byte_string {
                    return APQError::PersistedQueryNotSupported;
                }
            }
            _ => {
                return APQError::Other;
            }
        }
    }
    APQError::Other
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

#[cfg(test)]
mod tests {
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
    use serde_json_bytes::Value::Object;
    use tower::service_fn;
    use tower::ServiceExt;

    use super::*;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::query_planner::fetch::OperationKind;
    use crate::Context;
    use crate::SubgraphRequest;

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

    // starts a local server emulating a subgraph returning PERSISTED_QUERY_NOT_SUPPORTED error response
    async fn emulate_persisted_query_not_supported_response(socket_addr: SocketAddr) {
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
                                        .message("PersistedQueryNotSupported")
                                        .extension_code("PERSISTED_QUERY_NOT_SUPPORTED")
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

    // starts a local server emulating a subgraph returning PERSISTED_QUERY_NOT_FOUND error response
    async fn emulate_persisted_query_not_found_response(socket_addr: SocketAddr) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let mut apq_hash_store = HashMap::new();

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
                    let value = request
                        .extensions
                        .get(&ByteString::from(PERSISTED_QUERY_KEY))
                        .unwrap();
                    let hash;
                    match value {
                        Object(map) => hash = map.get(HASH_KEY),
                        _ => {
                            panic!("value should have been a map")
                        }
                    }
                    if apq_hash_store.contains_key(&hash) {
                        panic!("Should not have recieved a request whose hash \
                        is already known to server in this test")
                    }

                    if request.query == None {
                        return Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![Error::builder()
                                        .message("PersistedQueryNotFound")
                                        .extension_code("PERSISTED_QUERY_NOT_FOUND")
                                        .build()],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    } else {
                        apq_hash_store.insert(hash, request.query);
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
        let subgraph_service = SubgraphService::new("test", true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        let subgraph_service = SubgraphService::new("test", true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        let subgraph_service = SubgraphService::new("test", false);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        let subgraph_service = SubgraphService::new("test", true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
    async fn test_persisted_query_not_supported() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:2929").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_response(socket_addr));
        let subgraph_service = SubgraphService::new("test", true);

        assert_eq!(subgraph_service.clone().apq_enabled.as_ref().load(Relaxed), true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        assert_eq!(subgraph_service.apq_enabled.as_ref().load(Relaxed), false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_found() {
        let socket_addr = SocketAddr::from_str("127.0.0.1:3030").unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_response(socket_addr));
        let subgraph_service = SubgraphService::new("test", true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        let socket_addr = SocketAddr::from_str("127.0.0.1:3131").unwrap();
        tokio::task::spawn(emulate_expected_apq_enabled_configuration(socket_addr));
        let subgraph_service = SubgraphService::new("test", true);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
        let socket_addr = SocketAddr::from_str("127.0.0.1:3232").unwrap();
        tokio::task::spawn(emulate_expected_apq_disabled_configuration(socket_addr));
        let subgraph_service = SubgraphService::new("test", false);

        let url = Uri::from_str(&format!("http://{}", socket_addr)).unwrap();
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
}
