//! Service which makes individual requests to Apollo Connectors over some transport

use std::collections::HashMap;
use std::collections::HashSet;
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
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
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

/// A boxed service making a single connector request. This is what
/// [`PluginUnstable::connector_request_service`](crate::plugin::PluginUnstable::connector_request_service)
/// receives and returns, so a plugin wraps this type to customize connector traffic.
pub type BoxService = tower::util::BoxService<Request, Response, BoxError>;

/// The result of a single connector request.
pub type ServiceResult = Result<Response, BoxError>;

assert_impl_all!(Request: Send);
assert_impl_all!(Response: Send);

/// Request type for a single connector request
#[derive(Debug)]
pub struct Request {
    /// The request context, shared with the rest of the router pipeline for this
    /// operation. Readable and writable: a plugin may store values here for later
    /// stages, matching what the coprocessor `ConnectorRequest` stage can do.
    pub context: Context,

    /// The connector associated with this request.
    //
    // Deliberately kept `pub(crate)` now that this service is public: `Connector`
    // carries the full expanded connector definition, far more internal detail than a
    // customization needs. If plugins turn out to need something from it, expose that
    // through a narrow accessor or a purpose-built type rather than the whole struct.
    pub(crate) connector: Arc<Connector>,

    /// The request to the underlying transport.
    ///
    /// [`TransportRequest::Http`] holds the outgoing [`http::Request`], so a plugin can
    /// read and rewrite its URI, headers, body and method.
    ///
    /// Note that the method is writable here but *not* through the coprocessor
    /// `ConnectorRequest` stage, which sends the method to the coprocessor but ignores
    /// any method it sends back. A plugin that rewrites the method therefore has no
    /// coprocessor equivalent.
    pub transport_request: TransportRequest,

    /// Information about how to map the response to GraphQL
    pub(crate) key: ResponseKey,

    /// Mapping problems encountered when creating the transport request
    pub(crate) mapping_problems: Vec<Problem>,

    /// Original request to the Router. Read it through
    /// [`Request::supergraph_request`].
    pub(crate) supergraph_request: Arc<http::Request<graphql::Request>>,

    /// The operation being executed. Together with
    /// req.connector.schema_subtypes_map, this document enables GraphQL
    /// execution of the document.
    pub(crate) operation: Option<Arc<Valid<ExecutableDocument>>>,
}

impl Request {
    /// The original request made to the router, which produced this connector request.
    ///
    /// Read-only on purpose. `ConnectorRequestService::call` and its callees read this
    /// request while handling the connector call, and every other plugin on the chain
    /// sees the same value, so letting one plugin swap it out would change behaviour
    /// well outside that plugin. Rewrite [`Request::transport_request`] instead to
    /// change what goes over the wire.
    pub fn supergraph_request(&self) -> &Arc<http::Request<graphql::Request>> {
        &self.supergraph_request
    }

    /// Consume this request and produce a failed [`Response`] for it, without making
    /// the outbound call.
    ///
    /// This is the in-process equivalent of a coprocessor returning `Control::Break`
    /// from the `ConnectorRequest` stage: the connector call is not made, and `message`,
    /// `code`, and `extensions` are reported to the client as a GraphQL error at this
    /// connector's path (an entry for `code` in `extensions` is ignored). Use it to fail
    /// a connector request deliberately, for example to circuit break on an upstream a
    /// plugin knows to be unhealthy.
    ///
    /// The error's remaining fields are derived from the request and are not settable,
    /// for the same reason the coprocessor does not let a coprocessor set them: the
    /// path and response key are what merge the failure back into the right place in
    /// the GraphQL response.
    pub fn into_error_response(
        self,
        message: impl Into<String>,
        code: impl Into<String>,
        extensions: impl IntoIterator<Item = (impl Into<ByteString>, impl Into<Value>)>,
    ) -> Response {
        let message = message.into();
        let subgraph_name = self.connector.id.subgraph_name.to_string();
        let mut error = RuntimeError::new(message.clone(), &self.key).with_code(code);
        for (k, v) in extensions {
            let k = k.into();
            if k.as_str() != "code" {
                error = error.extension(k, v);
            }
        }

        Response {
            context: self.context,
            subgraph_name,
            transport_result: Err(Error::TransportFailure(message)),
            mapped_response: MappedResponse::Error {
                error,
                key: self.key,
                problems: Vec::new(),
            },
        }
    }
}

/// Response type for a connector
#[derive(Debug)]
pub struct Response {
    /// The request context, shared with the rest of the router pipeline for this
    /// operation. Readable and writable, matching what the coprocessor
    /// `ConnectorResponse` stage can do.
    pub context: Context,

    /// Originating federation subgraph name for this connector call. Carried
    /// on the response (rather than passed through shared context) so parallel
    /// connector calls don't race when resolving per-subgraph response rules.
    pub(crate) subgraph_name: String,

    /// The result of the transport request.
    ///
    /// This is the raw transport outcome: HTTP status, headers and transport-level
    /// errors. Telemetry and downstream plugins read it, but the data returned to the
    /// client comes from the mapped response, which is *not* recomputed when this
    /// changes. Rewriting the status or headers here therefore makes telemetry
    /// disagree with what the client actually receives unless you make the
    /// corresponding change through the mapped-response accessors.
    pub transport_result: Result<TransportResponse, Error>,

    /// The mapped response, including any mapping problems encountered when processing
    /// the response. This is what is merged into the GraphQL response returned to the
    /// client. Kept private so that only the parts a customization may safely change
    /// are reachable; see the accessors on [`Response`].
    pub(crate) mapped_response: MappedResponse,
}

impl Response {
    /// The mapped response data returned to the client, or `None` if this connector
    /// call produced an error instead. See [`Response::error`] for that case.
    pub fn data(&self) -> Option<&serde_json_bytes::Value> {
        match &self.mapped_response {
            MappedResponse::Data { data, .. } => Some(data),
            MappedResponse::Error { .. } => None,
        }
    }

    /// Replace the mapped response data returned to the client.
    ///
    /// Returns `false` and changes nothing if this is an error response: a
    /// customization cannot turn a failed connector call into a successful one, which
    /// is also true of the coprocessor `ConnectorResponse` stage.
    ///
    /// This does not touch [`Response::transport_result`], so telemetry continues to
    /// report the status and headers actually received from the upstream.
    pub fn set_data(&mut self, data: serde_json_bytes::Value) -> bool {
        match &mut self.mapped_response {
            MappedResponse::Data { data: current, .. } => {
                *current = data;
                true
            }
            MappedResponse::Error { .. } => false,
        }
    }

    /// The error returned to the client, or `None` if this connector call produced
    /// data instead. See [`Response::data`] for that case.
    pub fn error(&self) -> Option<&RuntimeError> {
        match &self.mapped_response {
            MappedResponse::Error { error, .. } => Some(error),
            MappedResponse::Data { .. } => None,
        }
    }

    /// Replace the message of the error returned to the client.
    ///
    /// Returns `false` and changes nothing if this is a successful response: a
    /// customization cannot turn a successful connector call into a failed one here,
    /// which is also true of the coprocessor `ConnectorResponse` stage. To fail a
    /// connector call, break it before it is made with
    /// [`Request::into_error_response`].
    pub fn set_error_message(&mut self, message: impl Into<String>) -> bool {
        match &mut self.mapped_response {
            MappedResponse::Error { error, .. } => {
                error.message = message.into();
                true
            }
            MappedResponse::Data { .. } => false,
        }
    }

    /// Replace the `code` extension of the error returned to the client.
    ///
    /// Returns `false` and changes nothing if this is a successful response, as for
    /// [`Response::set_error_message`].
    pub fn set_error_code(&mut self, code: impl Into<String>) -> bool {
        match &mut self.mapped_response {
            MappedResponse::Error { error, .. } => {
                error.set_code(code);
                true
            }
            MappedResponse::Data { .. } => false,
        }
    }

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
        connector_sources: Arc<HashSet<String>>,
    ) -> Self {
        let mut map = HashMap::with_capacity(connector_sources.len());
        for source in connector_sources.iter() {
            let service = UnconstrainedBuffer::new(
                plugins
                    .iter()
                    .rev()
                    .fold(
                        ConnectorRequestService {
                            http_client_service_factory: http_client_service_factory.clone(),
                        }
                        .boxed(),
                        |acc, (_, e)| e.connector_request_service(acc, source.clone()),
                    )
                    .boxed(),
                DEFAULT_BUFFER_SIZE,
            );
            map.insert(source.clone(), service);
        }

        Self {
            services: Arc::new(map), //connector_sources,
        }
    }

    pub(crate) fn create(&self, source_name: String) -> BoxService {
        // Note: We have to box our cloned service to erase the type of the Buffer.
        self.services
            .get(&source_name)
            .map(|svc| svc.clone().boxed())
            .expect("We should always get a service, even if it is a blank/default one")
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

                        let source_name = request.connector.source_config_key();

                        let result = if let Some(http_client_service_factory) =
                            http_client_service_factory.get(&source_name).cloned()
                        {
                            let (parts, body) = http_request.inner.into_parts();
                            let http_request =
                                http::Request::from_parts(parts, router::body::from_bytes(body));

                            http_client_service_factory
                                    .create(&original_subgraph_name)
                                    .oneshot(crate::services::http::HttpRequest {
                                        http_request,
                                        context: request.context.clone(),
                                    })
                                    .await
                                    .map(|result| result.http_response)
                                    .map_err(|e|
                                        // Note: this previously used `#[from] BoxError` but when we moved `Error` into the
                                        // `apollo-federation` crate, we could longer reference `BoxError` from there.
                                        Error::TransportFailure((replace_subgraph_name(e, &request.connector)).to_string())
                                    )
                        } else {
                            Err(Error::TransportFailure("no http client found".into()))
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
