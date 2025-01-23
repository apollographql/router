//! Service which makes individual requests to Apollo Connectors over some transport

use std::sync::Arc;
use std::task::Poll;

use apollo_federation::sources::connect::Connector;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use parking_lot::Mutex;
use static_assertions::assert_impl_all;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::info_span;

use crate::error::FetchError;
use crate::graphql;
use crate::graphql::ErrorExtension;
use crate::json_ext::Path;
use crate::layers::ServiceBuilderExt;
use crate::plugins::connectors::handle_responses::process_response;
use crate::plugins::connectors::handle_responses::MappedResponse;
use crate::plugins::connectors::make_requests::ResponseKey;
use crate::plugins::connectors::mapping::Problem;
use crate::plugins::connectors::plugin::debug::ConnectorContext;
use crate::plugins::connectors::plugin::debug::ConnectorDebugHttpRequest;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::plugins::connectors::tracing::CONNECTOR_TYPE_HTTP;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventRequest;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::consts::CONNECT_REQUEST_SPAN_NAME;
use crate::services::connect;
use crate::services::connector::request_service::transport::http::HttpRequest;
use crate::services::connector::request_service::transport::http::HttpResponse;
use crate::services::http::HttpClientServiceFactory;
use crate::services::new_service::ServiceFactory;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::Plugins;
use crate::Context;

pub(crate) mod transport;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

assert_impl_all!(Request: Send);
assert_impl_all!(Response: Send);

/// Request type for a single connector request
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Request {
    /// The request context
    pub(crate) context: Context,

    /// The connector associated with this request
    // If this service moves into the public API, consider whether this exposes too much
    // internal information about the connector. A new type may be needed which exposes only
    // what is necessary for customizations.
    pub(crate) connector: Arc<Connector>,

    /// The service name for this connector
    pub(crate) service_name: String,

    /// The request to the underlying transport
    pub(crate) transport_request: TransportRequest,

    /// Information about how to map the response to GraphQL
    pub(crate) key: ResponseKey,

    /// Mapping problems encountered when creating the transport request
    pub(crate) mapping_problems: Vec<Problem>,

    pub(crate) original_request: Arc<connect::Request>,
}

/// Response type for a connector
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct Response {
    /// The response context
    #[allow(dead_code)]
    pub(crate) context: Context,

    /// The connector associated with this response
    #[allow(dead_code)]
    pub(crate) connector: Arc<Connector>,

    /// The result of the transport request
    pub(crate) transport_result: Result<TransportResponse, Error>,

    /// The mapped response, including any mapping problems encountered when processing the response
    pub(crate) mapped_response: MappedResponse,
}

/// Request to an underlying transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum TransportRequest {
    /// A request to an HTTP transport
    Http(HttpRequest),
}

/// Response from an underlying transport
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum TransportResponse {
    /// A response from an HTTP transport
    Http(HttpResponse),
}

impl From<HttpRequest> for TransportRequest {
    fn from(value: HttpRequest) -> Self {
        Self::Http(value)
    }
}

impl From<HttpResponse> for TransportResponse {
    fn from(value: HttpResponse) -> Self {
        Self::Http(value)
    }
}

/// An error sending a connector request. This represents a problem with sending the request
/// to the connector, rather than an error returned from the connector itself.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(crate) enum Error {
    /// Request limit exceeded
    RequestLimitExceeded,

    /// {0}
    TransportFailure(#[from] BoxError),
}

impl Error {
    /// Create a GraphQL error from this error.
    #[must_use]
    pub(crate) fn to_graphql_error(
        &self,
        connector: Arc<Connector>,
        path: Option<Path>,
    ) -> crate::error::Error {
        use serde_json_bytes::*;

        let builder = graphql::Error::builder()
            .message(self.to_string())
            .extension_code(self.extension_code())
            .extension("service", connector.id.subgraph_name.clone())
            .extension(
                "connector",
                Value::Object(Map::from_iter([(
                    "coordinate".into(),
                    Value::String(connector.id.coordinate().into()),
                )])),
            );
        if let Some(path) = path {
            builder.path(path).build()
        } else {
            builder.build()
        }
    }
}

impl ErrorExtension for Error {
    fn extension_code(&self) -> String {
        match self {
            Self::RequestLimitExceeded => "REQUEST_LIMIT_EXCEEDED",
            Self::TransportFailure(_) => "HTTP_CLIENT_ERROR",
        }
        .to_string()
    }
}

#[derive(Clone)]
pub(crate) struct ConnectorRequestServiceFactory {
    pub(crate) http_client_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
    pub(crate) plugins: Arc<Plugins>,
}

impl ConnectorRequestServiceFactory {
    pub(crate) fn new(
        http_client_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
        plugins: Arc<Plugins>,
    ) -> Self {
        Self {
            http_client_service_factory,
            plugins,
        }
    }
}

impl ServiceFactory<Request> for ConnectorRequestServiceFactory {
    type Service = BoxService;

    fn create(&self) -> Self::Service {
        ServiceBuilder::new()
            .instrument(|_| {
                info_span!(
                    CONNECT_REQUEST_SPAN_NAME,
                    "otel.kind" = "INTERNAL",
                    "otel.status_code" = tracing::field::Empty,
                )
            })
            .service(
                self.plugins.iter().rev().fold(
                    ConnectorRequestService {
                        http_client_service_factory: self.http_client_service_factory.clone(),
                    }
                    .boxed(),
                    |acc, (_, e)| e.connector_request_service(acc),
                ),
            )
            .boxed()
    }
}

/// A service for executing individual requests to Apollo Connectors
#[derive(Clone)]
pub(crate) struct ConnectorRequestService {
    pub(crate) http_client_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
}

impl tower::Service<Request> for ConnectorRequestService {
    type Response = Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let original_subgraph_name = request.connector.id.subgraph_name.to_string();
        let http_client_service_factory = self.http_client_service_factory.clone();

        // Load the information needed from the context
        let (debug, connector_request_event, request_limit) =
            request.context.extensions().with_lock(|lock| {
                (
                    lock.get::<Arc<Mutex<ConnectorContext>>>().cloned(),
                    lock.get::<ConnectorEventRequest>().cloned(),
                    lock.get::<Arc<RequestLimits>>()
                        .map(|limits| {
                            limits.get(
                                (&request.connector.id).into(),
                                request.connector.max_requests,
                            )
                        })
                        .unwrap_or(None),
                )
            });

        let log_request_level = connector_request_event.and_then(|s| match s.0.condition() {
            Some(condition) => {
                if condition.lock().evaluate_request(&request) == Some(true) {
                    Some(s.0.level())
                } else {
                    None
                }
            }
            None => Some(s.0.level()),
        });

        Box::pin(async move {
            let mut debug_request: Option<ConnectorDebugHttpRequest> = None;
            let result = if request_limit.is_some_and(|request_limit| !request_limit.allow()) {
                Err(Error::RequestLimitExceeded)
            } else {
                let result = match request.transport_request {
                    TransportRequest::Http(http_request) => {
                        debug_request = http_request.debug;

                        let http_request = log_request(
                            http_request.inner,
                            log_request_level,
                            &request.connector.id.label,
                        )
                        .await?;

                        if let Some(http_client_service_factory) = http_client_service_factory
                            .get(&request.service_name)
                            .cloned()
                        {
                            http_client_service_factory
                                .create(&original_subgraph_name)
                                .oneshot(crate::services::http::HttpRequest {
                                    http_request,
                                    context: request.context.clone(),
                                })
                                .await
                                .map(|result| result.http_response)
                                .map_err(|e| replace_subgraph_name(e, &request.connector).into())
                        } else {
                            Err(Error::TransportFailure("no http client found".into()))
                        }
                    }
                };

                u64_counter!(
                    "apollo.router.operations.connectors",
                    "Total number of requests to connectors",
                    1,
                    "connector.type" = CONNECTOR_TYPE_HTTP,
                    "subgraph.name" = original_subgraph_name
                );

                result
            };

            Ok(process_response(
                result,
                request.key.clone(),
                request.connector,
                &request.context,
                debug_request,
                &debug,
                request.original_request,
            )
            .await)
        })
    }
}

/// Log an event for this request, if configured
async fn log_request(
    request: http::Request<RouterBody>,
    log_request_level: Option<EventLevel>,
    label: &str,
) -> Result<http::Request<RouterBody>, BoxError> {
    if let Some(level) = log_request_level {
        let (parts, body) = request.into_parts();

        let mut attrs = Vec::with_capacity(5);

        #[cfg(test)]
        let headers = {
            let mut headers: IndexMap<String, http::HeaderValue> = parts
                .headers
                .clone()
                .into_iter()
                .filter_map(|(name, val)| Some((name?.to_string(), val)))
                .collect();
            headers.sort_keys();
            headers
        };
        #[cfg(not(test))]
        let headers = parts.headers.clone();

        attrs.push(KeyValue::new(
            HTTP_REQUEST_HEADERS,
            opentelemetry::Value::String(format!("{:?}", headers).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_METHOD,
            opentelemetry::Value::String(parts.method.as_str().to_string().into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_URI,
            opentelemetry::Value::String(format!("{}", parts.uri).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_VERSION,
            opentelemetry::Value::String(format!("{:?}", parts.version).into()),
        ));
        let body_bytes = router::body::into_bytes(body).await?;
        attrs.push(KeyValue::new(
            HTTP_REQUEST_BODY,
            opentelemetry::Value::String(
                String::from_utf8(body_bytes.clone().to_vec())
                    .unwrap_or_default()
                    .into(),
            ),
        ));
        log_event(
            level,
            "connector.request",
            attrs,
            &format!("Request to connector {label:?}"),
        );

        Ok(http::Request::<RouterBody>::from_parts(
            parts,
            router::body::from_bytes(body_bytes),
        ))
    } else {
        Ok(request)
    }
}

/// Replace the internal subgraph name in an error with the connector label
fn replace_subgraph_name(err: BoxError, connector: &Connector) -> BoxError {
    match err.downcast::<FetchError>() {
        Ok(inner) => match *inner {
            FetchError::SubrequestHttpError {
                status_code,
                service: _,
                reason,
            } => Box::new(FetchError::SubrequestHttpError {
                status_code,
                service: connector.id.subgraph_source(),
                reason,
            }),
            _ => inner,
        },
        Err(e) => e,
    }
}
