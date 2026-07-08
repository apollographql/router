//! Service which makes individual requests to Apollo Connectors over some transport

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_federation::connectors::Connector;
use apollo_federation::connectors::runtime::debug::ConnectorContext;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::errors::RuntimeError;
#[cfg(test)]
use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_federation::connectors::runtime::key::ResponseKey;
use apollo_federation::connectors::runtime::mapping::Problem;
use apollo_federation::connectors::runtime::responses::MappedResponse;
use apollo_federation::connectors::runtime::responses::handle_mapping_only_response;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD;
use parking_lot::Mutex;
use static_assertions::assert_impl_all;
use tower::BoxError;
use tower::ServiceExt;

use crate::Context;
use crate::error::FetchError;
use crate::graphql;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::plugins::connectors::handle_responses::process_response;
use crate::plugins::connectors::request_limit::RequestLimits;
use crate::plugins::connectors::tracing::CONNECTOR_TYPE_HTTP;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_BODY;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_HEADERS;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_URI;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_VERSION;
use crate::plugins::telemetry::config_new::connector::events::ConnectorEventRequest;
use crate::plugins::telemetry::config_new::events::EventLevel;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::services::Plugins;
use crate::services::http::HttpClientServiceFactory;
use crate::services::router;

pub(crate) type BoxCloneService = tower::util::BoxCloneService<Request, Response, BoxError>;
pub(crate) type ServiceResult = Result<Response, BoxError>;

assert_impl_all!(Request: Send);
assert_impl_all!(Response: Send);

/// Request type for a single connector request
#[derive(Debug)]
pub(crate) struct Request {
    /// The request context
    pub(crate) context: Context,

    /// The connector associated with this request
    // If this service moves into the public API, consider whether this exposes too much
    // internal information about the connector. A new type may be needed which exposes only
    // what is necessary for customizations.
    pub(crate) connector: Arc<Connector>,

    /// The request to the underlying transport
    pub(crate) transport_request: TransportRequest,

    /// Information about how to map the response to GraphQL
    pub(crate) key: ResponseKey,

    /// Mapping problems encountered when creating the transport request
    pub(crate) mapping_problems: Vec<Problem>,

    /// Original request to the Router.
    pub(crate) supergraph_request: Arc<http::Request<graphql::Request>>,

    /// The operation being executed. Together with
    /// req.connector.schema_subtypes_map, this document enables GraphQL
    /// execution of the document.
    pub(crate) operation: Option<Arc<Valid<ExecutableDocument>>>,
}

/// Response type for a connector
#[derive(Debug)]
pub(crate) struct Response {
    /// The request context
    pub(crate) context: Context,

    /// Originating federation subgraph name for this connector call. Carried
    /// on the response (rather than passed through shared context) so parallel
    /// connector calls don't race when resolving per-subgraph response rules.
    pub(crate) subgraph_name: String,

    /// The result of the transport request
    pub(crate) transport_result: Result<TransportResponse, Error>,

    /// The mapped response, including any mapping problems encountered when processing the response
    pub(crate) mapped_response: MappedResponse,
}

impl Response {
    pub(crate) fn error_new(
        context: Context,
        subgraph_name: String,
        error: Error,
        message: impl Into<String>,
        response_key: ResponseKey,
    ) -> Self {
        let graphql_error = RuntimeError::new(message, &response_key).with_code(error.code());

        let mapped_response = MappedResponse::Error {
            error: graphql_error,
            key: response_key,
            problems: Vec::new(),
        };

        Self {
            context,
            subgraph_name,
            transport_result: Err(error),
            mapped_response,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_new(
        context: Context,
        response_key: ResponseKey,
        problems: Vec<Problem>,
        data: serde_json_bytes::Value,
        headers: Option<http::HeaderMap<http::HeaderValue>>,
    ) -> Self {
        let mapped_response = MappedResponse::Data {
            data: data.clone(),
            problems,
            key: response_key,
        };

        let mut response_builder = http::Response::builder();
        if let Some(headers) = headers {
            for (header_name, header_value) in headers.iter() {
                response_builder = response_builder.header(header_name, header_value);
            }
        }
        let (parts, _value) = response_builder.body(data).unwrap().into_parts();
        let http_response = HttpResponse { inner: parts };

        Self {
            context,
            subgraph_name: String::new(),
            transport_result: Ok(http_response.into()),
            mapped_response,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConnectorRequestServiceFactory {
    pub(crate) services:
        Arc<HashMap<String, UnconstrainedBuffer<Request, BoxFuture<'static, ServiceResult>>>>,
}

impl ConnectorRequestServiceFactory {
    pub(crate) fn new(
        http_client_service_factory: Arc<IndexMap<String, HttpClientServiceFactory>>,
        plugins: Arc<Plugins>,
    ) -> Self {
        // `http_client_service_factory` contains exactly one entry per connector
        // source (see `create_http_services` in router_factory.rs), so it doubles
        // as the set of connector sources to build buffers for.
        let mut map = HashMap::with_capacity(http_client_service_factory.len());
        for (source, factory) in http_client_service_factory.iter() {
            // source_config_key() format is "{subgraph_name}.{source_or_synthetic}";
            // the subgraph name is the first dot-separated component.
            let subgraph_name = source.split('.').next().unwrap_or(source);
            let http_client = factory.create(subgraph_name);
            // One buffer per connector source provides per-source backpressure and is
            // required for correct LoadShed / RateLimit behaviour from traffic-shaping
            // plugins (mirrors the per-subgraph buffer in SubgraphServiceFactory).
            let service = UnconstrainedBuffer::new(
                plugins.iter().rev().fold(
                    ConnectorRequestService { http_client }.boxed_clone(),
                    |acc, (_, e)| e.connector_request_service(acc, source.clone()),
                ),
                DEFAULT_BUFFER_SIZE,
            );
            map.insert(source.clone(), service);
        }

        Self {
            services: Arc::new(map),
        }
    }

    pub(crate) fn create(&self, source_name: String) -> BoxCloneService {
        // Note: We have to box our cloned service to erase the type of the Buffer.
        self.services
            .get(&source_name)
            .map(|svc| svc.clone().boxed_clone())
            .expect("We should always get a service, even if it is a blank/default one")
    }
}

/// A service for executing individual requests to Apollo Connectors
#[derive(Clone)]
pub(crate) struct ConnectorRequestService {
    /// Pre-built HTTP client service for this service's source, with all
    /// plugin layers already folded in. Each `ConnectorRequestService`
    /// instance only handles requests for a single source, so we store
    /// just the one client it needs.
    pub(crate) http_client: crate::services::http::BoxCloneService,
}

impl tower::Service<Request> for ConnectorRequestService {
    type Response = Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http_client.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let original_subgraph_name = request.connector.id.subgraph_name.to_string();
        let fresh_client = self.http_client.clone();
        let mut http_client = std::mem::replace(&mut self.http_client, fresh_client);

        // Load the information needed from the context
        let (debug, connector_request_event, request_limit) =
            request.context.extensions().with_lock(|lock| {
                (
                    lock.get::<Arc<Mutex<ConnectorContext>>>().cloned(),
                    lock.get::<ConnectorEventRequest>().cloned(),
                    lock.get::<Arc<RequestLimits>>()
                        .map(|limits| {
                            limits.get(
                                request.connector.as_ref().into(),
                                request.connector.max_requests,
                            )
                        })
                        .unwrap_or(None),
                )
            });

        let log_request_level = connector_request_event.and_then(|s| {
            if s.condition.lock().evaluate_request(&request) == Some(true) {
                Some(s.level)
            } else {
                None
            }
        });

        Box::pin(async move {
            match request.transport_request {
                // For mapping-only connectors, skip HTTP entirely and apply the selection against {}
                TransportRequest::MappingOnly => {
                    let mapped = handle_mapping_only_response(
                        request.key,
                        &request.connector,
                        &request.context,
                        request.supergraph_request.headers(),
                    )
                    .apply_operation(
                        request
                            .operation
                            .as_ref()
                            .map(|arc_valid_doc| arc_valid_doc.as_ref().as_ref()),
                        &request.connector.schema_subtypes_map,
                    );
                    if matches!(mapped, MappedResponse::Data { .. }) {
                        tracing::Span::current().record(
                            crate::plugins::telemetry::consts::OTEL_STATUS_CODE,
                            crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK,
                        );
                    }
                    Ok(Response {
                        context: request.context,
                        subgraph_name: original_subgraph_name,
                        transport_result: Ok(TransportResponse::MappingOnly),
                        mapped_response: mapped,
                    })
                }

                TransportRequest::Http(http_request) => {
                    let mut debug_request = (None, Default::default());
                    let result = if request_limit
                        .is_some_and(|request_limit| !request_limit.allow())
                    {
                        Err(Error::RequestLimitExceeded)
                    } else {
                        debug_request = http_request.debug;

                        log_request(
                            &http_request.inner,
                            log_request_level,
                            request.connector.label.as_ref(),
                            &request.context,
                            &original_subgraph_name,
                        );

                        let (parts, body) = http_request.inner.into_parts();
                        let http_request =
                            http::Request::from_parts(parts, router::body::from_bytes(body));

                        let result = http_client
                            .call(crate::services::http::HttpRequest {
                                http_request,
                                context: request.context.clone(),
                            })
                            .await
                            .map(|result| result.http_response)
                            .map_err(|e|
                                // Note: this previously used `#[from] BoxError` but when we moved `Error` into the
                                // `apollo-federation` crate, we could longer reference `BoxError` from there.
                                Error::TransportFailure((replace_subgraph_name(e, &request.connector)).to_string())
                            );

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
                        request.key,
                        request.connector,
                        &request.context,
                        debug_request,
                        debug.as_ref(),
                        request.supergraph_request,
                        request.operation.clone(),
                    )
                    .await)
                }
            }
        })
    }
}

/// Log an event for this request, if configured
fn log_request(
    request: &http::Request<String>,
    log_request_level: Option<EventLevel>,
    label: &str,
    context: &Context,
    subgraph_name: &str,
) {
    if let Some(level) = log_request_level {
        let mut attrs = Vec::with_capacity(5);

        let header_string = crate::services::header_masking::masked_headers_for_log(
            context,
            crate::services::header_masking::Direction::Request,
            Some(subgraph_name),
            request.headers(),
        );

        attrs.push(KeyValue::new(
            HTTP_REQUEST_HEADERS,
            opentelemetry::Value::String(header_string.into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_METHOD,
            opentelemetry::Value::String(request.method().as_str().to_string().into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_URI,
            opentelemetry::Value::String(format!("{}", request.uri()).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_VERSION,
            opentelemetry::Value::String(format!("{:?}", request.version()).into()),
        ));
        attrs.push(KeyValue::new(
            HTTP_REQUEST_BODY,
            opentelemetry::Value::String(request.body().clone().into()),
        ));
        log_event(
            level,
            "connector.request",
            attrs,
            &format!("Request to connector {label:?}"),
        );
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
