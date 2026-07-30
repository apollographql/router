//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::task::Poll;

use bytes::Bytes;
use futures::TryFutureExt;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Request;
use http::StatusCode;
use http::header;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::response::Parts;
use http_body::Body;
use http_body_util::LengthLimitError;
use hyper_rustls::ConfigBuilderExt;
use itertools::Itertools;
use mediatype::MediaType;
use mediatype::names::APPLICATION;
use mediatype::names::JSON;
use mime::APPLICATION_JSON;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use rustls::RootCertStore;
use serde_json_bytes::Entry;
use serde_json_bytes::json;
use tokio::sync::oneshot;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::instrument;

use super::Plugins;
use super::http::HttpClientServiceFactory;
use super::http::HttpRequest;
use super::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use super::router::body::RouterBody;
use super::subgraph::SubgraphRequestId;
use crate::Configuration;
use crate::Context;
use crate::Notify;
use crate::batching::BatchQuery;
use crate::batching::BatchQueryInfo;
use crate::batching::assemble_batch;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::configuration::TlsClientAuth;
use crate::error::FetchError;
use crate::error::SubgraphBatchingError;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::layers::unconstrained_buffer::UnconstrainedBufferLayer;
use crate::plugins::file_uploads;
use crate::plugins::limits::SubgraphResponseSizeLimit;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::events::log_subgraph_request_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventResponse;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphRequestBodySize;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphResponseBodySize;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::http::service::WireByteCount;
use crate::services::layers::apq;
use crate::services::router;
use crate::services::subgraph;

const PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_FOUND";
const PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE: &str = "PERSISTED_QUERY_NOT_SUPPORTED";
const PERSISTED_QUERY_NOT_FOUND_MESSAGE: &str = "PersistedQueryNotFound";
const PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE: &str = "PersistedQueryNotSupported";
const CODE_STRING: &str = "code";
const PERSISTED_QUERY_KEY: &str = "persistedQuery";
const HASH_VERSION_KEY: &str = "version";
const HASH_VERSION_VALUE: i32 = 1;
const HASH_KEY: &str = "sha256Hash";
const GRAPHQL_RESPONSE: mediatype::Name = mediatype::Name::new_unchecked("graphql-response");

#[allow(clippy::declare_interior_mutable_const)]
pub(crate) static APPLICATION_JSON_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("application/json");
static ACCEPT_GRAPHQL_JSON: HeaderValue =
    HeaderValue::from_static("application/json, application/graphql-response+json");

enum APQError {
    PersistedQueryNotSupported,
    PersistedQueryNotFound,
    Other,
}

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub(crate) struct SubgraphService {
    // we hold a HTTP client service factory here because a service with plugins applied
    // cannot be cloned
    pub(crate) client_factory: HttpClientServiceFactory,
    service: Arc<String>,

    /// Whether apq is enabled in the router for subgraph calls
    /// This is enabled by default can be configured as
    /// subgraph:
    ///      apq: <bool>
    /// If a subgraph sends the error message PERSISTED_QUERY_NOT_SUPPORTED,
    /// apq is set to false
    apq: Arc<AtomicBool>,
}

impl SubgraphService {
    pub(crate) fn from_config(
        service: impl Into<String>,
        configuration: &Configuration,
        client_factory: HttpClientServiceFactory,
    ) -> Result<Self, BoxError> {
        let name: String = service.into();

        let enable_apq = configuration
            .apq
            .subgraph
            .subgraphs
            .get(&name)
            .map(|apq| apq.enabled)
            .unwrap_or(configuration.apq.subgraph.all.enabled);

        SubgraphService::new(name, enable_apq, client_factory)
    }

    pub(crate) fn new(
        service: impl Into<String>,
        enable_apq: bool,
        client_factory: crate::services::http::HttpClientServiceFactory,
    ) -> Result<Self, BoxError> {
        Ok(Self {
            client_factory,
            service: Arc::new(service.into()),
            apq: Arc::new(<AtomicBool>::new(enable_apq)),
        })
    }
}

pub(crate) fn generate_tls_client_config(
    tls_cert_store: Option<RootCertStore>,
    client_cert_config: Option<&TlsClientAuth>,
) -> Result<rustls::ClientConfig, BoxError> {
    let tls_builder = rustls::ClientConfig::builder();
    Ok(match (tls_cert_store, client_cert_config) {
        (None, None) => tls_builder.with_native_roots()?.with_no_client_auth(),
        (Some(store), None) => tls_builder
            .with_root_certificates(store)
            .with_no_client_auth(),
        (None, Some(client_auth_config)) => {
            tls_builder.with_native_roots()?.with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone_key(),
            )?
        }
        (Some(store), Some(client_auth_config)) => tls_builder
            .with_root_certificates(store)
            .with_client_auth_cert(
                client_auth_config.certificate_chain.clone(),
                client_auth_config.key.clone_key(),
            )?,
    })
}

impl tower::Service<SubgraphRequest> for SubgraphService {
    type Response = SubgraphResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: SubgraphRequest) -> Self::Future {
        let service_name = (*self.service).to_owned();

        let client_factory = self.client_factory.clone();

        let arc_apq_enabled = self.apq.clone();

        let make_calls = async move {
            // If APQ is not enabled, simply make the graphql call
            // with the same request body.
            let apq_enabled = arc_apq_enabled.as_ref();
            if !apq_enabled.load(Relaxed) {
                return call_http(request, client_factory.clone(), &service_name).await;
            }

            // APQ works by sending the query hash via extensions with an empty query body.
            // We use query.take() to save the query in case it's needed for a retry.
            let body = request.subgraph_request.body_mut();
            let original_query = body.query.take();

            let hash_value =
                apq::calculate_hash_for_query(original_query.as_deref().unwrap_or_default());
            body.extensions.insert(
                PERSISTED_QUERY_KEY,
                serde_json_bytes::json!({
                    HASH_VERSION_KEY: HASH_VERSION_VALUE,
                    HASH_KEY: hash_value
                }),
            );

            let response =
                call_http(request.clone(), client_factory.clone(), &service_name).await?;

            // Check the error for the request with only persistedQuery.
            // If PersistedQueryNotSupported, disable APQ for this subgraph
            // If PersistedQueryNotFound, add the original query to the request and retry.
            // Else, return the response like before.
            let gql_response = response.response.body();
            match get_apq_error(gql_response) {
                APQError::PersistedQueryNotSupported => {
                    apq_enabled.store(false, Relaxed);
                    let body = request.subgraph_request.body_mut();
                    body.query = original_query;
                    // Remove the persistedQuery extension we added for the APQ attempt.
                    body.extensions.remove(PERSISTED_QUERY_KEY);
                    call_http(request, client_factory.clone(), &service_name).await
                }
                APQError::PersistedQueryNotFound => {
                    request.subgraph_request.body_mut().query = original_query;
                    call_http(request, client_factory.clone(), &service_name).await
                }
                _ => Ok(response),
            }
        };

        Box::pin(make_calls)
    }
}

// Utility function to extract uri details.
fn get_uri_details(uri: &hyper::Uri) -> (&str, u16, &str) {
    let port = uri.port_u16().unwrap_or_else(|| {
        let scheme = uri.scheme_str();
        if scheme == Some("https") {
            443
        } else if scheme == Some("http") {
            80
        } else {
            0
        }
    });

    (uri.host().unwrap_or_default(), port, uri.path())
}

// Utility function to create a graphql response from HTTP response components
fn http_response_to_graphql_response(
    service_name: &str,
    content_type: Result<ContentType, FetchError>,
    body: Option<Result<Bytes, FetchError>>,
    parts: &Parts,
) -> graphql::Response {
    let mut graphql_response = match (content_type, body, parts.status.is_success()) {
        (Ok(ContentType::ApplicationGraphqlResponseJson), Some(Ok(body)), _)
        | (Ok(ContentType::ApplicationJson), Some(Ok(body)), true) => {
            // Application graphql json expects valid graphql response
            // Application json expects valid graphql response if 2xx
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                graphql::Response::from_bytes(body).unwrap_or_else(|error| {
                    let error = FetchError::SubrequestMalformedResponse {
                        service: service_name.to_owned(),
                        reason: error.reason,
                    };
                    graphql::Response::builder()
                        .error(error.to_graphql_error(None))
                        .build()
                })
            })
        }
        (Ok(ContentType::ApplicationJson), Some(Ok(body)), false) => {
            // Application json does not expect a valid graphql response if not 2xx.
            // If parse fails then attach the entire payload as an error
            tracing::debug_span!("parse_subgraph_response").in_scope(|| {
                // Application graphql json expects valid graphql response
                let mut original_response = String::from_utf8_lossy(&body).to_string();
                if original_response.is_empty() {
                    original_response = "<empty response body>".into()
                }
                graphql::Response::from_bytes(body).unwrap_or_else(|_error| {
                    graphql::Response::builder()
                        .error(
                            FetchError::SubrequestMalformedResponse {
                                service: service_name.to_string(),
                                reason: original_response,
                            }
                            .to_graphql_error(None),
                        )
                        .build()
                })
            })
        }
        (content_type, body, _) => {
            // Something went wrong, compose a response with errors if they are present
            let mut graphql_response = graphql::Response::builder().build();
            if let Err(err) = content_type {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            if let Some(Err(err)) = body {
                graphql_response.errors.push(err.to_graphql_error(None));
            }
            graphql_response
        }
    };

    // Any errors directly parsed from the response likely won't yet have the service name set,
    // but we need it for telemetry error counting
    for err in &mut graphql_response.errors {
        if let Entry::Vacant(v) = err.extensions.entry("service") {
            v.insert(json!(service_name));
        }
    }

    // Add an error for response codes that are not 2xx
    if !parts.status.is_success() {
        let status = parts.status;
        graphql_response.errors.insert(
            0,
            FetchError::SubrequestHttpError {
                service: service_name.to_string(),
                status_code: Some(status.as_u16()),
                reason: format!(
                    "{}: {}",
                    status.as_str(),
                    status.canonical_reason().unwrap_or("Unknown")
                ),
            }
            .to_graphql_error(None),
        )
    }
    graphql_response
}

/// Process a single subgraph batch request
#[instrument(skip(client_factory, contexts, request))]
pub(crate) async fn process_batch(
    client_factory: HttpClientServiceFactory,
    service: String,
    mut contexts: Vec<(Context, SubgraphRequestId)>,
    mut request: http::Request<RouterBody>,
    listener_count: usize,
) -> Result<Vec<SubgraphResponse>, FetchError> {
    // Now we need to "batch up" our data and send it to our subgraphs
    request
        .headers_mut()
        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
    request
        .headers_mut()
        .append(ACCEPT, ACCEPT_GRAPHQL_JSON.clone());

    let schema_uri = request.uri();
    let (host, port, path) = get_uri_details(schema_uri);

    // We can't provide a single operation name in the span (since we may be processing multiple
    // operations). Product decision, use the hard coded value "batch".
    let subgraph_req_span = tracing::info_span!(SUBGRAPH_REQUEST_SPAN_NAME,
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %schema_uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %&service,
        "graphql.operation.name" = "batch",
        "apollo.subgraph.response.aborted" = tracing::field::Empty,
    );

    // The graphql spec is lax about what strategy to use for processing responses: https://github.com/graphql/graphql-over-http/blob/main/spec/GraphQLOverHTTP.md#processing-the-response
    //
    // "If the response uses a non-200 status code and the media type of the response payload is application/json
    // then the client MUST NOT rely on the body to be a well-formed GraphQL response since the source of the response
    // may not be the server but instead some intermediary such as API gateways, proxies, firewalls, etc."
    //
    // The TLDR of this is that it's really asking us to do the best we can with whatever information we have with some modifications depending on content type.
    // Our goal is to give the user the most relevant information possible in the response errors
    //
    // Rules:
    // 1. If the content type of the response is not `application/json` or `application/graphql-response+json` then we won't try to parse.
    // 2. If an HTTP status is not 2xx it will always be attached as a graphql error.
    // 3. If the response type is `application/json` and status is not 2xx and the body the entire body will be output if the response is not valid graphql.

    // We need a "representative context" for a batch. We use the first context in our list of
    // contexts
    let batch_context = contexts
        .first()
        .expect("we have at least one context in the batch")
        .0
        .clone();
    let client = client_factory.create(&service);

    // Update our batching metrics (just before we fetch)
    u64_histogram!(
        "apollo.router.operations.batching.size",
        "Number of queries contained within each query batch",
        listener_count as u64,
        mode = BatchingMode::BatchHttpLink.to_string(), // Only supported mode right now
        subgraph = service.clone()
    );

    u64_counter!(
        "apollo.router.operations.batching",
        "Total requests with batched operations",
        1,
        // XXX(@goto-bus-stop): Should these be `batching.mode`, `batching.subgraph`?
        // Also, other metrics use a different convention to report the subgraph name
        mode = BatchingMode::BatchHttpLink.to_string(), // Only supported mode right now
        subgraph = service.clone()
    );

    // Perform the actual fetch. If this fails then we didn't manage to make the call at all, so we can't do anything with it.
    tracing::debug!("fetching from subgraph: {service}");
    let (parts, content_type, body) = match do_fetch(client, &batch_context, &service, request)
        .instrument(subgraph_req_span)
        .await
    {
        Ok(res) => res,
        Err(err) => {
            let resp = http::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(err.to_graphql_error(None))
                .map_err(|err| FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service.clone(),
                    reason: format!("cannot create the http response from error: {err:?}"),
                })?;
            let (parts, body) = resp.into_parts();
            let body =
                serde_json::to_vec(&body).map_err(|err| FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service.clone(),
                    reason: format!("cannot serialize the error: {err:?}"),
                })?;
            (
                parts,
                Ok(ContentType::ApplicationJson),
                Some(Ok(body.into())),
            )
        }
    };

    // Mask sensitive response headers once, for reuse in both the telemetry
    // event and the debug log below. Logging the raw `parts` would otherwise
    // leak the very header values this masking redacts.
    let headers_str = crate::services::header_masking::masked_headers_for_log(
        &batch_context,
        crate::services::header_masking::Direction::Response,
        Some(&service),
        &parts.headers,
    );

    let subgraph_response_event = batch_context
        .extensions()
        .with_lock(|lock| lock.get::<SubgraphEventResponse>().cloned());
    if let Some(event) = subgraph_response_event {
        let mut attrs = Vec::with_capacity(5);
        attrs.push(KeyValue::new(
            Key::from_static_str("http.response.headers"),
            opentelemetry::Value::String(headers_str.clone().into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.response.status"),
            opentelemetry::Value::String(format!("{}", parts.status).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.response.version"),
            opentelemetry::Value::String(format!("{:?}", parts.version).into()),
        ));
        if let Some(Ok(b)) = &body {
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.body"),
                opentelemetry::Value::String(String::from_utf8_lossy(b).to_string().into()),
            ));
        }
        attrs.push(KeyValue::new(
            Key::from_static_str("subgraph.name"),
            opentelemetry::Value::String(service.clone().into()),
        ));
        log_event(
            event.level,
            "subgraph.response",
            attrs,
            &format!("Raw response from subgraph {service:?} received"),
        );
    }

    tracing::debug!(
        "parts status: {:?}, version: {:?}, headers: {headers_str}, content_type: {content_type:?}, body: {body:?}",
        parts.status,
        parts.version,
    );
    let value =
        serde_json::from_slice(&body.ok_or(FetchError::SubrequestMalformedResponse {
            service: service.to_string(),
            reason: "no body in response".to_string(),
        })??)
        .map_err(|error| FetchError::SubrequestMalformedResponse {
            service: service.to_string(),
            reason: error.to_string(),
        })?;

    tracing::debug!("json value from body is: {value:?}");

    let array = ensure_array!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
        service: service.to_string(),
        reason: error.to_string(),
    })?;
    let mut graphql_responses = Vec::with_capacity(array.len());
    for value in array {
        let object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service.to_string(),
                reason: error.to_string(),
            })?;

        // Map our Vec<u8> into Bytes
        // Map our serde conversion error to a FetchError
        let body = Some(
            serde_json::to_vec(&object)
                .map(|v| v.into())
                .map_err(|error| FetchError::SubrequestMalformedResponse {
                    service: service.to_string(),
                    reason: error.to_string(),
                }),
        );

        let graphql_response =
            http_response_to_graphql_response(&service, content_type.clone(), body, &parts);
        graphql_responses.push(graphql_response);
    }

    tracing::debug!("we have a vec of graphql_responses: {graphql_responses:?}");
    // Before we process our graphql responses, ensure that we have a context for each
    // response
    if graphql_responses.len() != contexts.len() {
        return Err(FetchError::SubrequestBatchingError {
            service,
            reason: format!(
                "number of contexts ({}) is not equal to number of graphql responses ({})",
                contexts.len(),
                graphql_responses.len()
            ),
        });
    }

    // We are going to pop contexts from the back, so let's reverse our contexts
    contexts.reverse();
    let subgraph_name = service.clone();
    // Build an http Response for each graphql response
    let subgraph_responses: Result<Vec<_>, _> = graphql_responses
        .into_iter()
        .map(|res| {
            let subgraph_name = subgraph_name.clone();
            http::Response::builder()
                .status(parts.status)
                .version(parts.version)
                .body(res)
                .map(|mut http_res| {
                    *http_res.headers_mut() = parts.headers.clone();
                    // Use the original context for the request to create the response
                    let (context, id) =
                        contexts.pop().expect("we have a context for each response");
                    let resp =
                        SubgraphResponse::new_from_response(http_res, context, subgraph_name, id);

                    // Avoid `{resp:?}`: SubgraphResponse's derived Debug prints
                    // the response HeaderMap unmasked. Log the non-header parts.
                    tracing::debug!(
                        "built subgraph response for {}: status={:?}, body={:?}",
                        resp.subgraph_name,
                        resp.response.status(),
                        resp.response.body(),
                    );
                    resp
                })
                .map_err(|e| FetchError::MalformedResponse {
                    reason: e.to_string(),
                })
        })
        .collect();

    // Avoid `{subgraph_responses:?}`: each SubgraphResponse's derived Debug
    // prints the response HeaderMap unmasked. Log a count (or the error).
    match &subgraph_responses {
        Ok(responses) => tracing::debug!("built {} subgraph responses", responses.len()),
        Err(error) => tracing::debug!("failed to build subgraph responses: {error}"),
    }
    subgraph_responses
}

/// Notify all listeners of a batch query of the results
pub(crate) async fn notify_batch_query(
    service: String,
    senders: Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
    responses: Result<Vec<SubgraphResponse>, FetchError>,
) -> Result<(), BoxError> {
    // Avoid `{responses:#?}`: SubgraphResponse's derived Debug prints the
    // response HeaderMap unmasked. Log the listener count and a result summary.
    match &responses {
        Ok(responses) => tracing::debug!(
            "handling response for service '{service}' with {} listeners: {} responses",
            senders.len(),
            responses.len(),
        ),
        Err(error) => tracing::debug!(
            "handling response for service '{service}' with {} listeners: error: {error}",
            senders.len(),
        ),
    }

    match responses {
        // If we had an error processing the batch, then pipe that error to all of the listeners
        Err(e) => {
            for tx in senders {
                // Try to notify all waiters. If we can't notify an individual sender, then log an error
                // which, unlike failing to notify on success (see below), contains the the entire error
                // response.
                if let Err(log_error) = tx.send(Err(Box::new(e.clone()))).map_err(|error| {
                    FetchError::SubrequestBatchingError {
                        service: service.clone(),
                        reason: format!("tx send failed: {error:?}"),
                    }
                }) {
                    tracing::error!(service, error=%log_error, "failed to notify sender that batch processing failed");
                }
            }
        }

        Ok(rs) => {
            // Before we process our graphql responses, ensure that we have a tx for each
            // response
            if senders.len() != rs.len() {
                return Err(Box::new(FetchError::SubrequestBatchingError {
                    service,
                    reason: format!(
                        "number of txs ({}) is not equal to number of graphql responses ({})",
                        senders.len(),
                        rs.len()
                    ),
                }));
            }

            // We have checked before we started looping that we had a tx for every
            // graphql_response, so zip_eq shouldn't panic.
            // Use the tx to send a graphql_response message to each waiter.
            for (response, sender) in rs.into_iter().zip_eq(senders) {
                if let Err(log_error) = sender
                    .send(Ok(response))
                    // If we fail to notify the waiter that our request succeeded, do not log
                    // out the entire response since this may be substantial and/or contain
                    // PII data. Simply log that the send failed.
                    .map_err(|_error| FetchError::SubrequestBatchingError {
                        service: service.to_string(),
                        reason: "tx send failed".to_string(),
                    })
                {
                    tracing::error!(service, error=%log_error, "failed to notify sender that batch processing succeeded");
                }
            }
        }
    }

    Ok(())
}

type BatchInfo = (
    (
        String,
        http::Request<RouterBody>,
        Vec<(Context, SubgraphRequestId)>,
        usize,
    ),
    Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
);

/// Collect all batch requests and process them concurrently
#[instrument(skip_all)]
pub(crate) async fn process_batches(
    client_factory: HttpClientServiceFactory,
    svc_map: HashMap<String, Vec<BatchQueryInfo>>,
) -> Result<(), BoxError> {
    // We need to strip out the senders so that we can work with them separately.
    let mut errors = vec![];
    let (info, txs): (Vec<_>, Vec<_>) =
        futures::future::join_all(svc_map.into_iter().map(|(service, requests)| async {
            let (_op_name, contexts, request, txs) = assemble_batch(requests).await?;

            Ok(((service, request, contexts, txs.len()), txs))
        }))
        .await
        .into_iter()
        .filter_map(|x: Result<BatchInfo, BoxError>| x.map_err(|e| errors.push(e)).ok())
        .unzip();

    // If errors isn't empty, then process_batches cannot proceed. Let's log out the errors and
    // return
    if !errors.is_empty() {
        for error in errors {
            tracing::error!("assembling batch failed: {error}");
        }
        return Err(SubgraphBatchingError::ProcessingFailed(
            "assembling batches failed".to_string(),
        )
        .into());
    }
    // Collect all of the processing logic and run them concurrently, collecting all errors
    let cf = &client_factory;
    // It is not ok to panic if the length of the txs and info do not match. Let's make sure they
    // do
    if txs.len() != info.len() {
        return Err(SubgraphBatchingError::ProcessingFailed(
            "length of txs and info are not equal".to_string(),
        )
        .into());
    }
    let batch_futures = info.into_iter().zip_eq(txs).map(
        |((service, request, contexts, listener_count), senders)| async move {
            let batch_result = process_batch(
                cf.clone(),
                service.clone(),
                contexts,
                request,
                listener_count,
            )
            .await;

            notify_batch_query(service, senders, batch_result).await
        },
    );

    futures::future::try_join_all(batch_futures).await?;

    Ok(())
}

async fn call_http(
    request: SubgraphRequest,
    client_factory: HttpClientServiceFactory,
    service_name: &str,
) -> Result<SubgraphResponse, BoxError> {
    // We use configuration to determine if calls may be batched. If we have Batching
    // configuration, then we check (batch_include()) if the current subgraph has batching enabled
    // in configuration. If it does, we then start to process a potential batch.
    //
    // If we are processing a batch, then we'd like to park tasks here, but we can't park them whilst
    // we have the context extensions lock held. That would be very bad...
    // We grab the (potential) BatchQuery and then operate on it later
    let opt_batch_query = request.context.extensions().with_lock(|lock| {
        lock.get::<Batching>()
            .and_then(|batching_config| batching_config.batch_include(service_name).then_some(()))
            .and_then(|_| lock.get::<BatchQuery>().cloned())
            .and_then(|bq| (!bq.finished()).then_some(bq))
    });

    // If we have a batch query, then it's time for batching
    if let Some(query) = opt_batch_query {
        // Let the owning batch know that this query is ready to process, getting back the channel
        // from which we'll eventually receive our response.
        let response_rx = query.signal_progress(client_factory, request).await?;

        // Park this query until we have our response and pass it back up
        response_rx
            .await
            .map_err(|err| FetchError::SubrequestBatchingError {
                service: service_name.to_string(),
                reason: format!("tx receive failed: {err}"),
            })?
    } else {
        tracing::debug!("we called http");
        let client = client_factory.create(service_name);
        call_single_http(request, client, service_name).await
    }
}

/// call_single_http makes http calls with modified graphql::Request (body)
pub(crate) async fn call_single_http(
    request: SubgraphRequest,
    client: crate::services::http::BoxService,
    service_name: &str,
) -> Result<SubgraphResponse, BoxError> {
    let subgraph_request_event = request
        .context
        .extensions()
        .with_lock(|lock| lock.get::<SubgraphEventRequest>().cloned());
    let log_request_level = subgraph_request_event.and_then(|s| {
        if s.condition.lock().evaluate_request(&request) == Some(true) {
            Some(s.level)
        } else {
            None
        }
    });

    let SubgraphRequest {
        subgraph_request,
        context,
        id: subgraph_request_id,
        ..
    } = request;

    let (parts, body) = subgraph_request.into_parts();
    let operation_name = body
        .operation_name
        .as_deref()
        .unwrap_or_default()
        .to_owned();
    let body = serde_json::to_string(&body)?;
    tracing::debug!("our JSON body: {body:?}");
    let mut request = http::Request::from_parts(parts, router::body::from_bytes(body));

    request
        .headers_mut()
        .insert(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone());
    request
        .headers_mut()
        .append(ACCEPT, ACCEPT_GRAPHQL_JSON.clone());

    let schema_uri = request.uri();
    let (host, port, path) = get_uri_details(schema_uri);

    let subgraph_req_span = tracing::info_span!(SUBGRAPH_REQUEST_SPAN_NAME,
        "otel.kind" = "CLIENT",
        "net.peer.name" = %host,
        "net.peer.port" = %port,
        "http.route" = %path,
        "http.url" = %schema_uri,
        "net.transport" = "ip_tcp",
        "apollo.subgraph.name" = %service_name,
        "graphql.operation.name" = %operation_name,
        "apollo.subgraph.response.aborted" = tracing::field::Empty,
    );

    // The graphql spec is lax about what strategy to use for processing responses: https://github.com/graphql/graphql-over-http/blob/main/spec/GraphQLOverHTTP.md#processing-the-response
    //
    // "If the response uses a non-200 status code and the media type of the response payload is application/json
    // then the client MUST NOT rely on the body to be a well-formed GraphQL response since the source of the response
    // may not be the server but instead some intermediary such as API gateways, proxies, firewalls, etc."
    //
    // The TLDR of this is that it's really asking us to do the best we can with whatever information we have with some modifications depending on content type.
    // Our goal is to give the user the most relevant information possible in the response errors
    //
    // Rules:
    // 1. If the content type of the response is not `application/json` or `application/graphql-response+json` then we won't try to parse.
    // 2. If an HTTP status is not 2xx it will always be attached as a graphql error.
    // 3. If the response type is `application/json` and status is not 2xx and the body the entire body will be output if the response is not valid graphql.

    // TODO: Temporary solution to plug FileUploads plugin until 'http_client' will be fixed https://github.com/apollographql/router/pull/4666
    let request = file_uploads::http_request_wrapper(request).await;

    if let Some(level) = log_request_level {
        log_subgraph_request_event(
            level,
            service_name,
            crate::services::header_masking::masked_headers_for_log(
                &context,
                crate::services::header_masking::Direction::Request,
                Some(service_name),
                request.headers(),
            ),
            request.method(),
            request.version(),
            format!("{:?}", request.body()),
            &format!("Request to subgraph {service_name:?}"),
        );
    }

    // By this point, the selectors for on_request have already run, so we store
    // the request body size in the context extensions so that any on_response selectors
    // can access it.
    if let Some(body_len) = request.size_hint().exact() {
        context.extensions().with_lock(|lock| {
            lock.insert::<SubgraphRequestBodySize>(SubgraphRequestBodySize(body_len));
        });
    }

    // Perform the actual fetch. If this fails then we didn't manage to make the call at all, so we can't do anything with it.
    let (parts, content_type, body) = match do_fetch(client, &context, service_name, request)
        .instrument(subgraph_req_span)
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            return Ok(SubgraphResponse::builder()
                .subgraph_name(service_name.to_string())
                .error(err.to_graphql_error(None))
                .status_code(StatusCode::INTERNAL_SERVER_ERROR)
                .context(context)
                .extensions(Object::default())
                .build());
        }
    };

    let subgraph_response_event = context
        .extensions()
        .with_lock(|lock| lock.get::<SubgraphEventResponse>().cloned());

    if let Some(subgraph_response_event) = subgraph_response_event {
        // We have to do this in order to use selectors
        let mut resp_builder = http::Response::builder()
            .status(parts.status)
            .version(parts.version);
        if let Some(headers) = resp_builder.headers_mut() {
            *headers = parts.headers.clone();
        }
        let subgraph_response = SubgraphResponse::new_from_response(
            resp_builder
                .body(graphql::Response::default())
                .expect("it won't fail everything is coming from an existing response"),
            context.clone(),
            service_name.to_owned(),
            subgraph_request_id.clone(),
        );

        let should_log = subgraph_response_event
            .condition
            .evaluate_response(&subgraph_response);
        if should_log {
            let mut attrs = Vec::with_capacity(5);
            let headers_str = crate::services::header_masking::masked_headers_for_log(
                &context,
                crate::services::header_masking::Direction::Response,
                Some(service_name),
                &parts.headers,
            );
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.headers"),
                opentelemetry::Value::String(headers_str.into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.status"),
                opentelemetry::Value::String(format!("{}", parts.status).into()),
            ));
            attrs.push(KeyValue::new(
                Key::from_static_str("http.response.version"),
                opentelemetry::Value::String(format!("{:?}", parts.version).into()),
            ));
            if let Some(Ok(b)) = &body {
                attrs.push(KeyValue::new(
                    Key::from_static_str("http.response.body"),
                    opentelemetry::Value::String(String::from_utf8_lossy(b).to_string().into()),
                ));
            }
            attrs.push(KeyValue::new(
                Key::from_static_str("subgraph.name"),
                opentelemetry::Value::String(service_name.to_string().into()),
            ));
            log_event(
                subgraph_response_event.level,
                "subgraph.response",
                attrs,
                &format!("Raw response from subgraph {service_name:?} received"),
            );
        }
    }

    if body.as_ref().is_some_and(|r| r.is_ok())
        && let Some(wire_size) = parts
            .extensions
            .get::<WireByteCount>()
            .map(|c| c.0.load(Relaxed))
    {
        context.extensions().with_lock(|lock| {
            lock.insert::<SubgraphResponseBodySize>(SubgraphResponseBodySize(wire_size));
        });
    }

    let graphql_response =
        http_response_to_graphql_response(service_name, content_type, body, &parts);

    let resp = http::Response::from_parts(parts, graphql_response);
    Ok(SubgraphResponse::new_from_response(
        resp,
        context,
        service_name.to_owned(),
        subgraph_request_id,
    ))
}

#[derive(Clone, Debug)]
enum ContentType {
    ApplicationJson,
    ApplicationGraphqlResponseJson,
}

fn get_graphql_content_type(service_name: &str, parts: &Parts) -> Result<ContentType, FetchError> {
    if let Some(raw_content_type) = parts.headers.get(header::CONTENT_TYPE) {
        let content_type = raw_content_type
            .to_str()
            .ok()
            .and_then(|str| MediaType::parse(str).ok());

        match content_type {
            Some(mime) if mime.ty == APPLICATION && mime.subty == JSON => {
                Ok(ContentType::ApplicationJson)
            }
            Some(mime)
                if mime.ty == APPLICATION
                    && mime.subty == GRAPHQL_RESPONSE
                    && mime.suffix == Some(JSON) =>
            {
                Ok(ContentType::ApplicationGraphqlResponseJson)
            }
            Some(mime) => Err(format!(
                "subgraph response contains unsupported content-type: {mime}",
            )),
            None => Err(format!(
                "subgraph response contains invalid 'content-type' header value {raw_content_type:?}",
            )),
        }
    } else {
        Err("subgraph response does not contain 'content-type' header".to_owned())
    }
    .map_err(|reason| FetchError::SubrequestHttpError {
        status_code: Some(parts.status.as_u16()),
        service: service_name.to_string(),
        reason: format!(
            "{}; expected content-type: {} or content-type: {}",
            reason,
            APPLICATION_JSON.essence_str(),
            GRAPHQL_JSON_RESPONSE_HEADER_VALUE
        ),
    })
}

async fn do_fetch(
    mut client: crate::services::http::BoxService,
    context: &Context,
    service_name: &str,
    request: Request<RouterBody>,
) -> Result<
    (
        Parts,
        Result<ContentType, FetchError>,
        Option<Result<Bytes, FetchError>>,
    ),
    FetchError,
> {
    let response = client
        .call(HttpRequest {
            http_request: request,
            context: context.clone(),
        })
        .map_err(|err| {
            tracing::error!(fetch_error = ?err);
            FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.to_string(),
                reason: err.to_string(),
            }
        })
        .await?;

    let (parts, body) = response.http_response.into_parts();

    let content_type = get_graphql_content_type(service_name, &parts);

    let response_size_limit = context
        .extensions()
        .with_lock(|e| e.get::<SubgraphResponseSizeLimit>().copied());

    let body = if content_type.is_ok() {
        let body_result = match response_size_limit {
            Some(SubgraphResponseSizeLimit(limit)) => {
                router::body::into_bytes_limited(body, limit)
                    .instrument(tracing::debug_span!("aggregate_response_data"))
                    .await
                    .map_err(|err| {
                        tracing::error!(fetch_error = ?err);
                        let reason = if err.downcast_ref::<LengthLimitError>().is_some() {
                            u64_counter!(
                                "apollo.router.limits.subgraph_response_size.exceeded",
                                "Number of subgraph responses aborted because they exceeded the configured response size limit",
                                1,
                                subgraph.name = service_name.to_string()
                            );
                            tracing::Span::current()
                                .record("apollo.subgraph.response.aborted", "response_size_limit");
                            format!("subgraph response body exceeded limit of {limit} bytes")
                        } else {
                            err.to_string()
                        };
                        FetchError::SubrequestHttpError {
                            status_code: Some(parts.status.as_u16()),
                            service: service_name.to_string(),
                            reason,
                        }
                    })
            }
            None => {
                router::body::into_bytes(body)
                    .instrument(tracing::debug_span!("aggregate_response_data"))
                    .await
                    .map_err(|err| {
                        tracing::error!(fetch_error = ?err);
                        FetchError::SubrequestHttpError {
                            status_code: Some(parts.status.as_u16()),
                            service: service_name.to_string(),
                            reason: err.to_string(),
                        }
                    })
            }
        };
        Some(body_result)
    } else {
        None
    };
    Ok((parts, content_type, body))
}

fn get_apq_error(gql_response: &graphql::Response) -> APQError {
    for error in &gql_response.errors {
        // Check if error message is an APQ error
        match error.message.as_str() {
            PERSISTED_QUERY_NOT_FOUND_MESSAGE => {
                return APQError::PersistedQueryNotFound;
            }
            PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE => {
                return APQError::PersistedQueryNotSupported;
            }
            _ => {}
        }
        // Check if extensions contains the APQ error in "code"
        if let Some(value) = error.extensions.get(CODE_STRING) {
            if value == PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE {
                return APQError::PersistedQueryNotFound;
            } else if value == PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE {
                return APQError::PersistedQueryNotSupported;
            }
        }
    }
    APQError::Other
}

#[derive(Clone)]
pub(crate) struct SubgraphServiceFactory {
    pub(crate) services: Arc<
        HashMap<
            String,
            UnconstrainedBuffer<subgraph::Request, BoxFuture<'static, subgraph::ServiceResult>>,
        >,
    >,
}

impl SubgraphServiceFactory {
    pub(crate) fn new(
        services: Vec<(String, Arc<dyn MakeSubgraphService>)>,
        plugins: Arc<Plugins>,
        notify: Notify<String, graphql::Response>,
        subscription_config: Option<Arc<SubscriptionConfig>>,
    ) -> Self {
        let mut map = HashMap::with_capacity(services.len());
        for (name, maker) in services.into_iter() {
            // We have to do a little dance here to insert the subscription layer at the right
            // place: *after* all user plugins, but *before* the subgraph service proper.
            let inner_service = ServiceBuilder::new()
                .layer(SubscriptionSubgraphLayer::new(
                    notify.clone(),
                    subscription_config.clone(),
                    Arc::from(name.clone()),
                ))
                .service(maker.make())
                .boxed();
            let service = ServiceBuilder::new()
                .layer(UnconstrainedBufferLayer::new(DEFAULT_BUFFER_SIZE))
                .service(
                    plugins
                        .iter()
                        .rev()
                        .fold(inner_service, |acc, (_, e)| e.subgraph_service(&name, acc)),
                );
            map.insert(name, service);
        }

        SubgraphServiceFactory {
            services: Arc::new(map),
        }
    }

    pub(crate) fn create(&self, name: &str) -> Option<subgraph::BoxService> {
        // Note: We have to box our cloned service to erase the type of the Buffer.
        self.services.get(name).map(|svc| svc.clone().boxed())
    }
}

/// make new instances of the subgraph service
///
/// there can be multiple instances of that service executing at any given time
pub(crate) trait MakeSubgraphService: Send + Sync + 'static {
    fn make(&self) -> subgraph::BoxCloneService;
}

impl<S> MakeSubgraphService for S
where
    S: Service<SubgraphRequest, Response = SubgraphResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <S as Service<SubgraphRequest>>::Future: Send,
{
    fn make(&self) -> subgraph::BoxCloneService {
        self.clone().boxed_clone()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use SubgraphRequest;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::extract::State;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::CloseFrame;
    use axum::extract::ws::Message;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use bytes::Buf;
    use futures::StreamExt;
    use http::StatusCode;
    use http::Uri;
    use http::header::HOST;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tower::Layer as _;
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::Context;
    use crate::assert_response_eq_ignoring_error_id;
    use crate::configuration::shared::Client as ClientConfiguration;
    use crate::configuration::subgraph::SubgraphConfiguration;
    use crate::graphql::Error;
    use crate::graphql::Request;
    use crate::graphql::Response;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::subscription::CallbackMode;
    use crate::plugins::subscription::HeartbeatInterval;
    use crate::plugins::subscription::SUBSCRIPTION_CALLBACK_HMAC_KEY;
    use crate::plugins::subscription::SubgraphPassthroughMode;
    use crate::plugins::subscription::SubscriptionModeConfig;
    use crate::plugins::subscription::WebSocketConfiguration;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
    use crate::plugins::subscription::subgraph::SubscriptionSubgraphService;
    use crate::protocols::websocket::ClientMessage;
    use crate::protocols::websocket::ServerError;
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::router;

    async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
    where
        Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
        Fut:
            std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
    {
        use hyper::body::Incoming;
        use hyper_util::rt::TokioExecutor;
        use hyper_util::rt::TokioIo;

        // Not sure this is the *right* place to do it, because it's actually clients that
        // use crypto, not the server.

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let handle = handle.clone();
            tokio::spawn(async move {
                // N.B. should use hyper service_fn here, since it's required to be implemented hyper Service trait!
                let svc = hyper::service::service_fn(|request: http::Request<Incoming>| {
                    handle(request.map(Body::new))
                });
                if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await
                {
                    eprintln!("server error: {err}");
                }
            });
        }
    }

    // starts a local server emulating a subgraph returning status code 400
    async fn emulate_subgraph_bad_request(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::BAD_REQUEST)
                .body(
                    serde_json::to_string(&Response {
                        errors: vec![
                            Error::builder()
                                .message("This went wrong")
                                .extension_code("FETCH_ERROR")
                                .build(),
                        ],
                        ..Response::default()
                    })
                    .expect("always valid")
                    .into(),
                )
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning status code 401
    async fn emulate_subgraph_unauthorized(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::UNAUTHORIZED)
                .body(r#""#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning connection closed
    #[cfg(not(target_os = "macos"))]
    async fn emulate_subgraph_panic(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            panic!("test")
        }

        let server = axum::serve(
            listener,
            Router::new().route("/", axum::routing::any_service(tower::service_fn(handle))),
        );
        server.await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_ok_status_invalid_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_invalid_response_invalid_status_application_json(
        listener: TcpListener,
    ) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::UNAUTHORIZED)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_invalid_response_invalid_status_application_graphql(
        listener: TcpListener,
    ) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                .status(StatusCode::UNAUTHORIZED)
                .body(r#"invalid"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_application_json_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(r#"{"data": null}"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning a large JSON response
    async fn emulate_subgraph_large_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            // 100 bytes of JSON — enough to exceed a small limit in tests
            let body = format!(r#"{{"data":{{"field":"{}"}}}}"#, "x".repeat(80));
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(body.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning bad response format
    async fn emulate_subgraph_application_graphql_response(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, GRAPHQL_JSON_RESPONSE_HEADER_VALUE)
                .status(StatusCode::OK)
                .body(r#"{"data": null}"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with missing content_type
    async fn emulate_subgraph_missing_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with invalid content_type
    async fn emulate_subgraph_invalid_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "application/json,application/json")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning unsupported content_type
    async fn emulate_subgraph_unsupported_content_type(listener: TcpListener) {
        async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            Ok(http::Response::builder()
                .header(CONTENT_TYPE, "text/html")
                .status(StatusCode::OK)
                .body(r#"TEST"#.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotSupported",...],...}
    async fn emulate_persisted_query_not_supported_message(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
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
                                    errors: vec![
                                        Error::builder()
                                            .message(PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE)
                                            .extension_code("Random code")
                                            .build(),
                                    ],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }

                    Ok(http::Response::builder()
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
                        .unwrap())
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_SUPPORTED"}],...}
    async fn emulate_persisted_query_not_supported_extension_code(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
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
                                    errors: vec![
                                        Error::builder()
                                            .message("Random message")
                                            .extension_code(
                                                PERSISTED_QUERY_NOT_SUPPORTED_EXTENSION_CODE,
                                            )
                                            .build(),
                                    ],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap());
                    }

                    Ok(http::Response::builder()
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
                        .unwrap())
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {["message": "PersistedQueryNotFound",...],...}
    async fn emulate_persisted_query_not_found_message(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!(
                            "Recieved request without persisted query in persisted_query_not_found test."
                        )
                    }

                    if request.query.is_none() {
                        Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![
                                        Error::builder()
                                            .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                                            .extension_code("Random Code")
                                            .build(),
                                    ],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap())
                    } else {
                        Ok(http::Response::builder()
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
                            .unwrap())
                    }
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning response with
    // "errors" : {[..., "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}],...}
    async fn emulate_persisted_query_not_found_extension_code(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!(
                            "Recieved request without persisted query in persisted_query_not_found test."
                        )
                    }

                    if request.query.is_none() {
                        Ok(http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body(
                                serde_json::to_string(&Response {
                                    data: Some(Value::String(ByteString::from("test"))),
                                    errors: vec![
                                        Error::builder()
                                            .message("Random message")
                                            .extension_code(
                                                PERSISTED_QUERY_NOT_FOUND_EXTENSION_CODE,
                                            )
                                            .build(),
                                    ],
                                    ..Response::default()
                                })
                                .expect("always valid")
                                .into(),
                            )
                            .unwrap())
                    } else {
                        Ok(http::Response::builder()
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
                            .unwrap())
                    }
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request with apq
    // and panics if it does not find a persistedQuery.
    async fn emulate_expected_apq_enabled_configuration(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if !request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!("persistedQuery expected when configuration has apq_enabled=true")
                    }

                    Ok(http::Response::builder()
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
                        .unwrap())
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    // starts a local server emulating a subgraph returning a response to request without apq
    // and panics if it finds a persistedQuery.
    async fn emulate_expected_apq_disabled_configuration(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (_, body) = request.into_parts();
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");

            match graphql_request {
                Ok(request) => {
                    if request.extensions.contains_key(PERSISTED_QUERY_KEY) {
                        panic!(
                            "persistedQuery not expected when configuration has apq_enabled=false"
                        )
                    }

                    Ok(http::Response::builder()
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
                        .unwrap())
                }
                Err(_) => {
                    panic!("invalid graphql request recieved")
                }
            }
        }

        serve(listener, handle).await.unwrap();
    }

    async fn emulate_correct_websocket_server(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            let res = ws.protocols(["graphql-transport-ws"]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
                assert!(matches!(subscribe_msg, ClientMessage::Subscribe { .. }));
                let client_id = if let ClientMessage::Subscribe { payload, id } = subscribe_msg {
                    assert_eq!(
                        payload,
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build()
                    );

                    id
                } else {
                    panic!("subscribe message should be sent");
                };

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id, payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    async fn emulate_incorrect_websocket_server(listener: TcpListener) {
        async fn ws_handler(
            _ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            Ok((http::StatusCode::BAD_REQUEST, "bad request"))
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// WebSocket server that sends one event then completes the subscription,
    /// causing the subgraph to naturally close the stream.
    async fn emulate_websocket_server_that_completes(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws.protocols(["graphql-transport-ws"]).on_upgrade(move |mut socket| async move {
                let connection_ack = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let ack_msg: ClientMessage = serde_json::from_str(&connection_ack).unwrap();
                assert!(matches!(ack_msg, ClientMessage::ConnectionInit { .. }));

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                    ))
                    .await
                    .unwrap();
                let new_message = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                let subscribe_msg: ClientMessage = serde_json::from_str(&new_message).unwrap();
                let client_id = if let ClientMessage::Subscribe { payload, id } = subscribe_msg {
                    assert_eq!(
                        payload,
                        Request::builder()
                            .query("subscription {\n  userWasCreated {\n    username\n  }\n}")
                            .build()
                    );
                    id
                } else {
                    panic!("subscribe message should be sent");
                };

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Next { id: client_id.clone(), payload: graphql::Response::builder().data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}})).build() }).unwrap(),
                    ))
                    .await
                    .unwrap();

                socket
                    .send(Message::text(
                        serde_json::to_string(&ServerMessage::Complete { id: client_id }).unwrap(),
                    ))
                    .await
                    .unwrap();
            });

            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    static CALLBACK_PROTOCOL_ACCEPT: HeaderValue =
        HeaderValue::from_static("application/json;callbackSpec=1.0");

    async fn emulate_subgraph_with_callback_data(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let (parts, body) = request.into_parts();
            assert!(
                parts
                    .headers
                    .get_all(ACCEPT)
                    .iter()
                    .any(|header_value| header_value == CALLBACK_PROTOCOL_ACCEPT)
            );
            let graphql_request: Result<graphql::Request, &str> = router::body::into_bytes(body)
                .await
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_reader(bytes.reader()).map_err(|_| ()))
                .map_err(|_| "failed to parse the request body as JSON");
            let graphql_request = graphql_request.unwrap();
            assert!(graphql_request.extensions.contains_key("subscription"));
            let subscription_extension: crate::plugins::subscription::subgraph::SubscriptionExtension = serde_json_bytes::from_value(
                graphql_request
                    .extensions
                    .get("subscription")
                    .unwrap()
                    .clone(),
            )
            .unwrap();
            assert_eq!(
                subscription_extension.callback_url.to_string(),
                format!(
                    "http://localhost:4000/testcallback/{}",
                    subscription_extension.subscription_id
                )
            );
            assert_eq!(subscription_extension.heartbeat_interval_ms, 0);

            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .body(
                    serde_json::to_string(&Response::builder().data(Value::Null).build())
                        .expect("always valid")
                        .into(),
                )
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    fn subscription_config() -> SubscriptionConfig {
        SubscriptionConfig {
            enabled: true,
            mode: SubscriptionModeConfig {
                callback: Some(CallbackMode {
                    public_url: Url::parse("http://localhost:4000/testcallback").unwrap(),
                    listen: None,
                    path: Some("/testcallback".to_string()),
                    subgraphs: vec![String::from("testbis")].into_iter().collect(),
                    heartbeat_interval: HeartbeatInterval::new_disabled(),
                }),
                passthrough: Some(SubgraphPassthroughMode {
                    all: None,
                    subgraphs: [(
                        "test".to_string(),
                        WebSocketConfiguration {
                            path: Some(String::from("/ws")),
                            protocol: WebSocketProtocol::default(),
                            heartbeat_interval: HeartbeatInterval::new_disabled(),
                            max_reconnect_attempts: 0,
                            reconnect_delay: None,
                        },
                    )]
                    .into(),
                }),
            },
            deduplication: SubgraphConfiguration::default(),
            max_opened_subscriptions: None,
            queue_capacity: None,
            max_lifetime: None,
        }
    }

    /// Manually add the subgraph subscription layer for subscriptions tests.
    /// This would otherwise be done by the SubgraphServiceFactory, but many unit tests do not use
    /// it.
    fn with_subscription_layer(s: SubgraphService) -> SubscriptionSubgraphService<SubgraphService> {
        SubscriptionSubgraphLayer::new(
            Notify::builder().build(),
            Some(Arc::new(subscription_config())),
            Arc::from(s.service.to_string()),
        )
        .layer(s)
    }

    fn supergraph_request(query: &str) -> Arc<http::Request<Request>> {
        Arc::new(
            http::Request::builder()
                .header(HOST, "host")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(Request::builder().query(query).build())
                .expect("expecting valid request"),
        )
    }

    fn subgraph_http_request(uri: Uri, query: &str) -> http::Request<Request> {
        http::Request::builder()
            .header(HOST, "rhost")
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .uri(uri)
            .body(Request::builder().query(query).build())
            .expect("expecting valid request")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_callback() {
        let _ = SUBSCRIPTION_CALLBACK_HMAC_KEY.set(String::from("TESTEST"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_subgraph_with_callback_data(listener));
        let subgraph_service = with_subscription_layer(
            SubgraphService::new(
                "testbis",
                true,
                HttpClientServiceFactory::from_config(
                    "testbis",
                    &Configuration::default(),
                    crate::configuration::shared::Client::default(),
                ),
            )
            .expect("can create a SubgraphService"),
        );
        let (tx, _rx) = mpsc::channel(2);
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        response.response.body().errors.iter().for_each(|e| {
            println!("error: {}", e.message);
        });
        assert!(response.response.body().errors.is_empty());
        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_content_type_application_graphql() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_graphql_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_content_type_application_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_json_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(target_os = "macos"))]
    async fn test_subgraph_service_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_panic(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(!response.response.body().errors.is_empty());
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': HTTP fetch failed from 'test': connection closed before message completed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_invalid_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_ok_status_invalid_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "service 'test' response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_response_size_limit_exceeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_large_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let context = Context::new();
        context
            .extensions()
            .with_lock(|e| e.insert(SubgraphResponseSizeLimit(10)));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(context)
                    .build(),
            )
            .await
            .unwrap();

        let errors = &response.response.body().errors;
        assert!(!errors.is_empty(), "expected an error for exceeded limit");
        assert!(
            errors[0].message.contains("exceeded limit of 10 bytes"),
            "unexpected error message: {}",
            errors[0].message
        );
        assert_eq!(
            errors[0].extensions.get("code").and_then(|v| v.as_str()),
            Some("SUBREQUEST_HTTP_ERROR")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_response_size_limit_under() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_application_json_response(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let context = Context::new();
        // Limit of 1000 bytes — well above {"data": null} (14 bytes)
        context
            .extensions()
            .with_lock(|e| e.insert(SubgraphResponseSizeLimit(1000)));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(context)
                    .build(),
            )
            .await
            .unwrap();

        assert!(
            response.response.body().errors.is_empty(),
            "expected no errors when response is under the limit"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_json(listener),
        );
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "service 'test' response was malformed: invalid"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_graphql() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_graphql(listener),
        );
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "service 'test' response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_correct_websocket_server(listener));
        let subgraph_service = with_subscription_layer(
            SubgraphService::new(
                "test",
                true,
                HttpClientServiceFactory::from_config(
                    "test",
                    &Configuration::default(),
                    crate::configuration::shared::Client::default(),
                ),
            )
            .expect("can create a SubgraphService"),
        );
        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);

        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();
        let message = gql_stream.next().await.unwrap();
        assert_eq!(
            message,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );
        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket_with_error() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            tokio::task::spawn(emulate_incorrect_websocket_server(listener));
            let subgraph_service = with_subscription_layer(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
            );
            let (tx, _rx) = mpsc::channel(2);

            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
            let err = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap_err();

            let err_str = err.to_string();
            assert!(err_str.starts_with("Websocket fetch failed from 'test': cannot connect websocket to subgraph: WebSocket upgrade failed. Status: 400 Bad Request; Headers: [\"content-type\": \"text/plain; charset=utf-8\"; \"content-length\": \"11\";"));

            assert_counter!(
                "apollo.router.operations.subscriptions.rejected",
                1,
                "reason" = "subgraph",
                "subgraph.name" = "test"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket_ended_increments_counter() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let spawned_task =
                tokio::task::spawn(emulate_websocket_server_that_completes(listener));
            let subgraph_service = with_subscription_layer(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
            );
            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);

            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();
            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();
            let message = gql_stream.next().await.unwrap();
            assert_eq!(
                message,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Drain remaining messages until the stream ends (server sent Complete)
            while gql_stream.next().await.is_some() {}

            // The ended counter is incremented in a spawned task after gql_stream
            // completes forwarding; yield briefly to let it run.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// Verifies that a server-sent Complete message does NOT trigger reconnection, even when
    /// max_reconnect_attempts > 0. A Complete ends the stream with a terminal `None`, which is
    /// never treated as a recoverable drop.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_complete_does_not_reconnect() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let spawned_task =
                tokio::task::spawn(emulate_websocket_server_that_completes(listener));

            // Configure reconnect — the Complete should suppress it entirely.
            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                5,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // One event from the server before Complete.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Stream ends cleanly — no reconnect attempt, no second event. The forwarding task
            // increments its metrics strictly before closing `handle_sink` (the event that
            // produces this `None`), so no extra wait is needed here.
            assert!(gql_stream.next().await.is_none());

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // Exactly one completion event for the logical subscription, even across the (here,
            // single) physical connection.
            assert_counter!(
                "apollo.router.operations.subscriptions.events",
                1,
                subscriptions.mode = "passthrough",
                subscriptions.complete = true
            );
            // Reconnect counter must remain zero.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// WebSocket server that tracks connection count via shared atomic.
    /// - First connection: sends one event then drops with an abnormal close (triggers reconnect).
    /// - Subsequent connections: sends one event and stays open (simulates successful reconnect).
    async fn emulate_websocket_server_with_reconnect(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        ws.protocols(["graphql-transport-ws"])
                            .on_upgrade(move |mut socket| async move {
                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                assert!(matches!(
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                    ClientMessage::ConnectionInit { .. }
                                ));
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::ConnectionAck)
                                            .unwrap(),
                                    ))
                                    .await
                                    .unwrap();

                                let msg = socket
                                    .recv()
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .into_text()
                                    .unwrap();
                                let client_id = if let ClientMessage::Subscribe { id, .. } =
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                {
                                    id
                                } else {
                                    panic!("expected Subscribe message");
                                };

                                let username =
                                    if conn_num == 0 { "ada_lovelace" } else { "grace_hopper" };
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::Next {
                                            id: client_id.clone(),
                                            payload: graphql::Response::builder()
                                                .data(serde_json_bytes::json!({"userWasCreated": {"username": username}}))
                                                .build(),
                                        })
                                        .unwrap(),
                                    ))
                                    .await
                                    .unwrap();

                                if conn_num == 0 {
                                    // Simulate unexpected connection drop with an abnormal close
                                    // frame (code 1011), which surfaces as a `Disconnected` event.
                                    // A Normal close or Complete would end the stream with a
                                    // terminal `None` and suppress reconnection.
                                    socket
                                        .send(Message::Close(Some(CloseFrame {
                                            code: 1011,
                                            reason: "unexpected termination".into(),
                                        })))
                                        .await
                                        .unwrap();
                                }
                                // Subsequent connections: hold open until the test aborts the task.
                            })
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// First connection: completes the handshake, sends one event, then drops with an abnormal
    /// close (triggering a reconnect). Every subsequent connection refuses the WebSocket upgrade
    /// (HTTP 500), so the reconnect handshake fails inside `open_ws_gql_stream`. Used to verify a
    /// failed *reconnect* handshake does not increment the `rejected` counter.
    async fn emulate_websocket_server_rejects_reconnect(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        if conn_num > 0 {
                            // Refuse the upgrade so the reconnect handshake fails.
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no upgrade")
                                .into_response();
                        }
                        ws.protocols(["graphql-transport-ws"])
                            .on_upgrade(move |mut socket| async move {
                                let msg =
                                    socket.recv().await.unwrap().unwrap().into_text().unwrap();
                                assert!(matches!(
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                    ClientMessage::ConnectionInit { .. }
                                ));
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::ConnectionAck)
                                            .unwrap(),
                                    ))
                                    .await
                                    .unwrap();
                                let msg =
                                    socket.recv().await.unwrap().unwrap().into_text().unwrap();
                                let client_id = if let ClientMessage::Subscribe { id, .. } =
                                    serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                {
                                    id
                                } else {
                                    panic!("expected Subscribe message");
                                };
                                socket
                                    .send(Message::text(
                                        serde_json::to_string(&ServerMessage::Next {
                                            id: client_id,
                                            payload: graphql::Response::builder()
                                                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                                .build(),
                                        })
                                        .unwrap(),
                                    ))
                                    .await
                                    .unwrap();
                                socket
                                    .send(Message::Close(Some(CloseFrame {
                                        code: 1011,
                                        reason: "unexpected termination".into(),
                                    })))
                                    .await
                                    .unwrap();
                            })
                            .into_response()
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Like `emulate_websocket_server_that_completes` but simulates an unexpected connection drop
    /// (abnormal close frame) instead of a protocol-level Complete. Used to test reconnect logic:
    /// the drop surfaces as a `Disconnected` event rather than a terminal `None`.
    async fn emulate_websocket_server_that_drops(listener: TcpListener) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws
                .protocols(["graphql-transport-ws"])
                .on_upgrade(move |mut socket| async move {
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    assert!(matches!(
                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                        ClientMessage::ConnectionInit { .. }
                    ));
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let client_id =
                        if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::Next {
                                id: client_id,
                                payload: graphql::Response::builder()
                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                    .build(),
                            })
                            .unwrap(),
                        ))
                        .await
                        .unwrap();
                    // Abnormal close — surfaces as a `Disconnected` event, triggering reconnect.
                    socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 1011,
                            reason: "unexpected termination".into(),
                        })))
                        .await
                        .unwrap();
                });
            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// First connection: completes the full graphql-ws handshake, sends one event, then
    /// drops with an abnormal close frame (triggering reconnect logic). Every subsequent
    /// connection stalls — the axum handler returns `pending` forever so the TCP connection
    /// is established but the HTTP upgrade response never arrives. Used to verify that the
    /// `subscription_closing_signal` select! arms abort the reconnect before the handshake
    /// completes (or even before the delay expires).
    async fn emulate_websocket_server_drops_then_stalls(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        let app = Router::new()
            .route(
                "/ws",
                get(
                    |ws: WebSocketUpgrade,
                     ConnectInfo(_addr): ConnectInfo<SocketAddr>,
                     State(count): State<Arc<AtomicU32>>| async move {
                        let conn_num = count.fetch_add(1, Ordering::SeqCst);
                        if conn_num > 0 {
                            // Stall: never send an HTTP response, so connect_async hangs.
                            std::future::pending::<axum::response::Response>().await
                        } else {
                            ws.protocols(["graphql-transport-ws"])
                                .on_upgrade(move |mut socket| async move {
                                    let msg = socket
                                        .recv()
                                        .await
                                        .unwrap()
                                        .unwrap()
                                        .into_text()
                                        .unwrap();
                                    assert!(matches!(
                                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                                        ClientMessage::ConnectionInit { .. }
                                    ));
                                    socket
                                        .send(Message::text(
                                            serde_json::to_string(&ServerMessage::ConnectionAck)
                                                .unwrap(),
                                        ))
                                        .await
                                        .unwrap();
                                    let msg = socket
                                        .recv()
                                        .await
                                        .unwrap()
                                        .unwrap()
                                        .into_text()
                                        .unwrap();
                                    let client_id =
                                        if let ClientMessage::Subscribe { id, .. } =
                                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                                        {
                                            id
                                        } else {
                                            panic!("expected Subscribe message");
                                        };
                                    socket
                                        .send(Message::text(
                                            serde_json::to_string(&ServerMessage::Next {
                                                id: client_id,
                                                payload: graphql::Response::builder()
                                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                                    .build(),
                                            })
                                            .unwrap(),
                                        ))
                                        .await
                                        .unwrap();
                                    socket
                                        .send(Message::Close(Some(CloseFrame {
                                            code: 1011,
                                            reason: "unexpected termination".into(),
                                        })))
                                        .await
                                        .unwrap();
                                })
                                .into_response()
                        }
                    },
                ),
            )
            .with_state(connection_count);

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Like `emulate_websocket_server_that_drops` but holds each connection open for
    /// `hold` after sending its single event, then drops with an abnormal close. The
    /// connection count is reported back so the test can stop the server once it has
    /// observed enough reconnect cycles.
    async fn emulate_websocket_server_stable_then_drops(
        listener: TcpListener,
        hold: std::time::Duration,
        max_drops: u32,
        connection_count: Arc<AtomicU32>,
    ) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
            State((hold, max_drops, connection_count)): State<(
                std::time::Duration,
                u32,
                Arc<AtomicU32>,
            )>,
        ) -> Result<impl IntoResponse, Infallible> {
            let res = ws
                .protocols(["graphql-transport-ws"])
                .on_upgrade(move |mut socket| async move {
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    assert!(matches!(
                        serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                        ClientMessage::ConnectionInit { .. }
                    ));
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                        ))
                        .await
                        .unwrap();
                    let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                    let client_id =
                        if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                    socket
                        .send(Message::text(
                            serde_json::to_string(&ServerMessage::Next {
                                id: client_id,
                                payload: graphql::Response::builder()
                                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                                    .build(),
                            })
                            .unwrap(),
                        ))
                        .await
                        .unwrap();
                    // Keep the connection open long enough for the router to treat it
                    // as stable (past the grace window).
                    tokio::time::sleep(hold).await;
                    // Drop only the first `max_drops` connections; hold any later connection open
                    // so the reconnect count is bounded and the test is deterministic (no extra
                    // reconnect can race the assertion).
                    let drop_index = connection_count.fetch_add(1, Ordering::SeqCst);
                    if drop_index < max_drops {
                        socket
                            .send(Message::Close(Some(CloseFrame {
                                code: 1011,
                                reason: "unexpected termination".into(),
                            })))
                            .await
                            .unwrap();
                    } else {
                        // Hold open until the test aborts the task.
                        std::future::pending::<()>().await;
                    }
                });
            Ok(res)
        }

        let app = Router::new().route("/ws", get(ws_handler)).with_state((
            hold,
            max_drops,
            connection_count,
        ));
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    /// Completes the handshake, sends a terminal operation `Error` (application-level, not a
    /// transport error code), then drops the connection with an abnormal close. The terminal
    /// Error must prevent the router from reconnecting even though the close is abnormal.
    async fn emulate_websocket_server_sends_error_then_drops(
        listener: TcpListener,
        connection_count: Arc<AtomicU32>,
    ) {
        async fn ws_handler(
            ws: WebSocketUpgrade,
            ConnectInfo(_addr): ConnectInfo<SocketAddr>,
            State(count): State<Arc<AtomicU32>>,
        ) -> Result<impl IntoResponse, Infallible> {
            count.fetch_add(1, Ordering::SeqCst);
            let res =
                ws.protocols(["graphql-transport-ws"])
                    .on_upgrade(move |mut socket| async move {
                        let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                        assert!(matches!(
                            serde_json::from_str::<ClientMessage>(&msg).unwrap(),
                            ClientMessage::ConnectionInit { .. }
                        ));
                        socket
                            .send(Message::text(
                                serde_json::to_string(&ServerMessage::ConnectionAck).unwrap(),
                            ))
                            .await
                            .unwrap();
                        let msg = socket.recv().await.unwrap().unwrap().into_text().unwrap();
                        let client_id = if let ClientMessage::Subscribe { id, .. } =
                            serde_json::from_str::<ClientMessage>(&msg).unwrap()
                        {
                            id
                        } else {
                            panic!("expected Subscribe message");
                        };
                        // Terminal operation error from the subgraph (not a transport error code).
                        socket
                            .send(Message::text(
                                serde_json::to_string(&ServerMessage::Error {
                                    id: Some(client_id),
                                    payload: ServerError::Error(
                                        Error::builder()
                                            .message("boom")
                                            .extension_code("MY_SUBGRAPH_ERROR")
                                            .build(),
                                    ),
                                })
                                .unwrap(),
                            ))
                            .await
                            .unwrap();
                        // Abnormal close after the terminal error — must NOT trigger a reconnect.
                        socket
                            .send(Message::Close(Some(CloseFrame {
                                code: 1011,
                                reason: "unexpected termination".into(),
                            })))
                            .await
                            .unwrap();
                    });
            Ok(res)
        }

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(connection_count);
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        server.await.unwrap();
    }

    fn subscription_config_with_reconnect(max_reconnect_attempts: u32) -> SubscriptionConfig {
        subscription_config_with_reconnect_delay(
            max_reconnect_attempts,
            std::time::Duration::from_millis(1),
        )
    }

    fn subscription_config_with_reconnect_delay(
        max_reconnect_attempts: u32,
        reconnect_delay: std::time::Duration,
    ) -> SubscriptionConfig {
        // Reconnect policy now lives on the per-subgraph WebSocketConfiguration; set it on the
        // "test" subgraph's passthrough config.
        let mut config = subscription_config();
        if let Some(passthrough) = &mut config.mode.passthrough
            && let Some(ws) = passthrough.subgraphs.get_mut("test")
        {
            ws.max_reconnect_attempts = max_reconnect_attempts;
            ws.reconnect_delay = Some(reconnect_delay);
        }
        config
    }

    fn with_subscription_layer_reconnect(
        s: SubgraphService,
        max_reconnect_attempts: u32,
    ) -> SubscriptionSubgraphService<SubgraphService> {
        SubscriptionSubgraphLayer::new(
            crate::plugins::subscription::notification::Notify::builder().build(),
            Some(Arc::new(subscription_config_with_reconnect(
                max_reconnect_attempts,
            ))),
            Arc::from(s.service.to_string()),
        )
        .layer(s)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicU32::new(0));
        let spawned_task = tokio::task::spawn(emulate_websocket_server_with_reconnect(
            listener,
            connection_count.clone(),
        ));

        let subgraph_service = with_subscription_layer_reconnect(
            SubgraphService::new(
                "test",
                true,
                HttpClientServiceFactory::from_config(
                    "test",
                    &Configuration::default(),
                    crate::configuration::shared::Client::default(),
                ),
            )
            .expect("can create a SubgraphService"),
            1,
        );

        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);
        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();

        // First event comes from the initial connection.
        let first = gql_stream.next().await.unwrap();
        assert_eq!(
            first,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );

        // Transient transport errors from the abnormal close are suppressed during the
        // reconnect window (so HTTP-multipart clients don't tear down). The next item the
        // client sees is data from the reconnected stream. Loop defensively in case any
        // unexpected error item slips through.
        let second = loop {
            let item = gql_stream.next().await.unwrap();
            if item.errors.is_empty() {
                break item;
            }
        };
        assert_eq!(
            second,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "grace_hopper"}}))
                .build()
        );

        spawned_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_exhausted_increments_counter() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            // emulate_websocket_server_that_drops sends one event then an abnormal close frame
            // on every connection, so every attempt (initial + reconnects) triggers reconnect logic.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let gql_stream = rx_stream.next().await;
            assert!(
                gql_stream.is_some(),
                "expected subscription stream from channel"
            );
            let mut gql_stream = gql_stream.unwrap();

            // Event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");
            assert_eq!(
                first.unwrap(),
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Errors from the first abnormal close are suppressed during the reconnect
            // window; the next item should be data from the reconnected stream.
            let second = loop {
                let item = gql_stream.next().await;
                assert!(item.is_some(), "stream ended before second data event");
                let item = item.unwrap();
                if item.errors.is_empty() {
                    break item;
                }
            };
            assert_eq!(
                second,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // Drain remaining items (errors from second drop) until the stream terminates. The
            // forwarding task increments its metrics strictly before closing `handle_sink` (the
            // event that ends this stream), so no extra wait is needed once it's drained.
            while gql_stream.next().await.is_some() {}

            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                1,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// Verifies the default behavior: when `max_reconnect_attempts` is 0 (equivalent to the
    /// unset default via `unwrap_or(0)` in subgraph.rs), an abnormal subgraph disconnect must
    /// terminate the subscription immediately — no reconnect attempt, no reconnect counter.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_drop_does_not_reconnect_when_attempts_zero() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            // Sends one event and then an abnormal close on every connection. With
            // max_reconnect_attempts=0 we expect only the first event to reach the client.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                0,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Event from the initial (and only) connection.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // After the abnormal close the stream must terminate without producing another data
            // item. Windows may surface one or more error items from the close frame before the
            // stream ends; drain those and assert no data follows.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => {
                        panic!("unexpected data after subgraph drop with max_reconnect_attempts=0")
                    }
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // The drop is the terminal end of the subscription on the subgraph side.
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // No reconnect was attempted.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// A connection that stays open past the grace window before dropping should
    /// refresh the per-disconnect retry budget. With `max_reconnect_attempts=1`,
    /// a server that drops after every "stable" connection should produce *more
    /// than one* reconnect — the budget resets on each drop. A hard lifetime
    /// ceiling would terminate after exactly one reconnect.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_budget_resets_after_stable_connection() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // The grace floor is 500ms (see `stability_grace` in subgraph.rs); hold each
            // connection 600ms so every drop is past it. Drop only the first 2 connections, then
            // hold the third open — so exactly 2 reconnects happen and no later reconnect can race
            // the assertion below.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_stable_then_drops(
                listener,
                std::time::Duration::from_millis(600),
                2,
                connection_count.clone(),
            ));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Pull data events through several reconnect cycles. After observing
            // 3 successful "stable" connections (one initial + 2 reconnects), we
            // know the budget was refreshed at least once — a hard ceiling would
            // have terminated after the first reconnect with max_reconnect_attempts=1.
            let mut data_events = 0u32;
            while data_events < 3 {
                let item = gql_stream.next().await.unwrap();
                if !item.errors.is_empty() {
                    continue;
                }
                assert_eq!(item.subscribed, Some(true));
                data_events += 1;
            }

            // The reconnect counter is incremented strictly before the reconnected stream's data
            // is forwarded to this client, so observing the 3rd data event above already
            // guarantees both reconnects are reflected in the metrics — no extra wait is needed.

            // Exactly 2 reconnects happened to reach 3 data events (the third connection is held
            // open, so no further reconnect occurs). With a hard ceiling on attempts
            // (`max_reconnect_attempts=1`), the counter would be capped at 1 and the subscription
            // would have terminated before the third event.
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                2,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// A failed *reconnect* handshake must increment the reconnect counter but NOT the
    /// `rejected` counter, which tracks rejected subscription requests rather than reconnect
    /// failures. The initial connect succeeds (so `rejected` is never touched there); the single
    /// reconnect attempt fails the WebSocket upgrade.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_failed_reconnect_does_not_increment_rejected() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            let spawned_task = tokio::task::spawn(emulate_websocket_server_rejects_reconnect(
                listener,
                connection_count.clone(),
            ));

            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                1,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Event from the initial (successful) connection.
            let first = gql_stream.next().await.unwrap();
            assert_eq!(
                first,
                graphql::Response::builder()
                    .subscribed(true)
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()
            );

            // The reconnect handshake fails; after the single attempt is exhausted the stream
            // terminates. Drain any error items, then assert the stream ends.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => panic!("unexpected data after failed reconnect"),
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // One reconnect attempt was issued (and failed).
            assert_counter!(
                "apollo.router.operations.subscriptions.reconnect",
                1,
                "subgraph.name" = "test"
            );
            // The subscription ultimately terminated subgraph-side.
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            // A failed reconnect handshake must NOT be counted as a rejected subscription request.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.rejected",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When reconnection is exhausted, the last suppressed transport error must be forwarded to
    /// the client so a failed subscription is distinguishable from a normal completion. The server
    /// drops every connection with an abnormal close, so after `max_reconnect_attempts` the
    /// subscription ends with a terminal transport error rather than a silent stream end.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_exhausted_forwards_terminal_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_websocket_server_that_drops(listener));

        let subgraph_service = with_subscription_layer_reconnect(
            SubgraphService::new(
                "test",
                true,
                HttpClientServiceFactory::from_config(
                    "test",
                    &Configuration::default(),
                    crate::configuration::shared::Client::default(),
                ),
            )
            .expect("can create a SubgraphService"),
            1,
        );

        let (tx, rx) = mpsc::channel(2);
        let mut rx_stream = ReceiverStream::new(rx);
        let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request(
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .subgraph_request(subgraph_http_request(
                        url,
                        "subscription {\n  userWasCreated {\n    username\n  }\n}",
                    ))
                    .operation_kind(OperationKind::Subscription)
                    .subscription_stream(tx)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert!(response.response.body().errors.is_empty());

        let mut gql_stream = rx_stream.next().await.unwrap();

        // Consume the whole stream. Transport errors are suppressed during the reconnect window;
        // only the terminal one (after attempts are exhausted) should reach the client.
        let mut data_events = 0u32;
        let mut terminal_error = None;
        while let Some(item) = gql_stream.next().await {
            if item.errors.is_empty() {
                data_events += 1;
            } else {
                terminal_error = Some(item);
            }
        }

        assert!(data_events >= 1, "expected at least the initial data event");
        let terminal_error = terminal_error
            .expect("client should receive a terminal error after reconnect exhausted");
        assert_eq!(terminal_error.subscribed, Some(false));
        // The terminal error is whatever transport error ended the last connection. An abnormal
        // close surfaces as WEBSOCKET_CLOSE_ERROR on most platforms, but can arrive as a read
        // failure (WEBSOCKET_MESSAGE_ERROR) depending on socket timing (e.g. on Windows).
        assert!(
            terminal_error.errors.iter().any(|e| matches!(
                e.extension_code().as_deref(),
                Some("WEBSOCKET_CLOSE_ERROR") | Some("WEBSOCKET_MESSAGE_ERROR")
            )),
            "terminal error should carry a transport error code, got: {:?}",
            terminal_error.errors
        );

        spawned_task.abort();
    }

    /// A terminal operation `Error` from the subgraph ends the subscription server-side. Even
    /// though the subgraph then drops the connection abnormally, the router must NOT reconnect
    /// (the Error marks the stream server-ended, so the following close is the expected teardown),
    /// and the client must receive the application error.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_application_error_does_not_reconnect() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            let spawned_task = tokio::task::spawn(emulate_websocket_server_sends_error_then_drops(
                listener,
                connection_count.clone(),
            ));

            // Reconnect is configured — the terminal Error must suppress it entirely.
            let subgraph_service = with_subscription_layer_reconnect(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
                5,
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // The client must receive the application error.
            let app_error = gql_stream.next().await.unwrap();
            assert!(
                app_error
                    .errors
                    .iter()
                    .any(|e| e.extension_code().as_deref() == Some("MY_SUBGRAPH_ERROR")),
                "client should receive the subgraph application error"
            );

            // After the terminal error the stream ends; no data from a reconnected stream.
            loop {
                match gql_stream.next().await {
                    Some(item) if !item.errors.is_empty() => continue,
                    Some(_) => panic!("unexpected data after a terminal application error"),
                    None => break,
                }
            }

            // The forwarding task increments its metrics strictly before closing `handle_sink`
            // (the event that ended the stream drained above), so no extra wait is needed here.

            // Exactly one connection was made — no reconnect was attempted.
            assert_eq!(connection_count.load(Ordering::SeqCst), 1);
            assert_counter!(
                "apollo.router.operations.subscriptions.terminated.subgraph",
                1,
                "subgraph.name" = "test"
            );
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When all clients drop while the router is sleeping during the reconnect delay, the
    /// router must abort without attempting the reconnect handshake at all. This exercises
    /// the `biased select!` arm on `subscription_closing_signal` inside the delay sleep.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_closing_signal_during_delay() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // Server stalls on the second connection; if the router incorrectly attempts
            // a reconnect the stall will hold the test open and the counter assertion below
            // will fail.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_drops_then_stalls(
                listener,
                connection_count.clone(),
            ));

            // Use a long reconnect delay (200 ms) so the test can reliably drop the client
            // stream before the delay expires and the reconnect handshake starts.
            let reconnect_delay = std::time::Duration::from_millis(200);
            let subgraph_service = SubscriptionSubgraphLayer::new(
                crate::plugins::subscription::notification::Notify::builder().build(),
                Some(Arc::new(subscription_config_with_reconnect_delay(
                    3,
                    reconnect_delay,
                ))),
                Arc::from("test"),
            )
            .layer(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Receive the first (and only) event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");

            // Drop the stream — simulates all clients disconnecting.
            // The router is now sleeping in the 200 ms reconnect delay; the closing signal
            // should interrupt it before the delay expires.
            drop(gql_stream);

            // Wait past the configured reconnect delay, not just "a few ms": a broken
            // implementation that ignores the closing signal would sleep the full delay and then
            // dial the stalling server, incrementing `connection_count` almost immediately after
            // (the mock increments it on TCP accept, before the handshake stalls). A wait shorter
            // than `reconnect_delay` can't tell "aborted correctly" apart from "still sleeping".
            tokio::time::sleep(reconnect_delay + std::time::Duration::from_millis(300)).await;

            // The closing signal must have fired during the delay sleep — no reconnect
            // handshake should have been issued.
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );
            // The server should never have seen a second connection.
            assert_eq!(
                connection_count.load(Ordering::SeqCst),
                1,
                "router must not attempt a reconnect after all clients disconnect"
            );
            // NB: the `subscriptions.events{complete=true}` emission also happens on this
            // client-disconnect teardown, but it races this assertion point (the client stream is
            // dropped rather than awaited to completion, so there's no synchronization with the
            // forwarding task's teardown). That emission is asserted deterministically in
            // `test_websocket_complete_does_not_reconnect` instead.

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    /// When all clients drop while the reconnect handshake is in progress (TCP connected,
    /// waiting for the HTTP upgrade response), the router must abort without completing
    /// the handshake and must NOT increment the reconnect counter. This exercises the
    /// `biased select!` arm on `subscription_closing_signal` inside `open_ws_gql_stream`.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_websocket_reconnect_closing_signal_during_handshake() {
        async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let connection_count = Arc::new(AtomicU32::new(0));
            // Server stalls on the second connection's HTTP upgrade so that the reconnect
            // handshake hangs long enough for the test to drop the client stream.
            let spawned_task = tokio::task::spawn(emulate_websocket_server_drops_then_stalls(
                listener,
                connection_count.clone(),
            ));

            // Use a very short reconnect delay so the handshake starts almost immediately
            // after the connection drops; the test then drops the client stream while the
            // handshake is in progress.
            let subgraph_service = SubscriptionSubgraphLayer::new(
                crate::plugins::subscription::notification::Notify::builder().build(),
                Some(Arc::new(subscription_config_with_reconnect_delay(
                    3,
                    std::time::Duration::from_millis(1),
                ))),
                Arc::from("test"),
            )
            .layer(
                SubgraphService::new(
                    "test",
                    true,
                    HttpClientServiceFactory::from_config(
                        "test",
                        &Configuration::default(),
                        crate::configuration::shared::Client::default(),
                    ),
                )
                .expect("can create a SubgraphService"),
            );

            let (tx, rx) = mpsc::channel(2);
            let mut rx_stream = ReceiverStream::new(rx);
            let url = Uri::from_str(&format!("ws://{socket_addr}")).unwrap();

            let response = subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .subgraph_request(subgraph_http_request(
                            url,
                            "subscription {\n  userWasCreated {\n    username\n  }\n}",
                        ))
                        .operation_kind(OperationKind::Subscription)
                        .subscription_stream(tx)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap();
            assert!(response.response.body().errors.is_empty());

            let mut gql_stream = rx_stream.next().await.unwrap();

            // Receive the first event from the initial connection.
            let first = gql_stream.next().await;
            assert!(first.is_some(), "stream ended before initial data event");

            // Wait long enough for the 1 ms reconnect delay to expire and for
            // `open_ws_gql_stream` to initiate a TCP connection to the stalling server,
            // then drop the stream to fire the closing signal mid-handshake.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // Confirm the handshake is actually in flight before we drop the stream — otherwise
            // the assertions below would pass trivially because the handshake was never attempted.
            assert_eq!(
                connection_count.load(Ordering::SeqCst),
                2,
                "expected the reconnect handshake to have dialed the stalling server by now"
            );
            drop(gql_stream);

            // Give the closing signal time to propagate through the notification task and be
            // picked up by the biased select! inside open_ws_gql_stream.
            //
            // NB: because `emulate_websocket_server_drops_then_stalls` never completes the
            // handshake, `open_ws_gql_stream` — and thus the `reconnect` counter increment, which
            // only fires after it returns — would also never resolve if the closing signal were
            // ignored. No amount of waiting here can distinguish "aborted correctly" from "still
            // hung on the stalled handshake" via this counter alone; the counter check below
            // mainly guards against a regression that increments it speculatively before the
            // handshake completes. `test_websocket_reconnect_closing_signal_during_delay` is the
            // test that deterministically proves the closing signal aborts in-flight reconnects.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // The handshake was aborted by the closing signal — the counter must NOT have
            // been incremented (it only fires after open_ws_gql_stream returns).
            assert_counter_not_exists!(
                "apollo.router.operations.subscriptions.reconnect",
                u64,
                "subgraph.name" = "test"
            );

            spawned_task.abort();
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code_should_not_fail() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': 400: Bad Request"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "This went wrong"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_missing_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_missing_content_type(listener));

        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_invalid_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_invalid_content_type(listener));

        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': subgraph response contains invalid 'content-type' header value \"application/json,application/json\"; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unsupported_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unsupported_content_type(listener));

        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': subgraph response contains unsupported content-type: text/html; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unauthorized() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unauthorized(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.response.body().errors[0].message,
            "HTTP fetch failed from 'test': 401: Unauthorized"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_supported_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_message(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
        assert!(!subgraph_service.apq.as_ref().load(Relaxed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_supported_extension_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_supported_extension_code(
            listener,
        ));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        assert!(subgraph_service.clone().apq.as_ref().load(Relaxed));

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
        assert!(!subgraph_service.apq.as_ref().load(Relaxed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_found_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_message(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_persisted_query_not_found_extension_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_persisted_query_not_found_extension_code(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_expected_apq_enabled_configuration(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            true,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_expected_apq_disabled_configuration(listener));
        let subgraph_service = SubgraphService::new(
            "test",
            false,
            HttpClientServiceFactory::from_config(
                "test",
                &Configuration::default(),
                crate::configuration::shared::Client::default(),
            ),
        )
        .expect("can create a SubgraphService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = subgraph_service
            .clone()
            .oneshot(
                SubgraphRequest::builder()
                    .supergraph_request(supergraph_request("query"))
                    .subgraph_request(subgraph_http_request(url, "query"))
                    .operation_kind(OperationKind::Query)
                    .subgraph_name(String::from("test"))
                    .context(Context::new())
                    .build(),
            )
            .await
            .unwrap();

        let expected_resp = Response {
            data: Some(Value::String(ByteString::from("test"))),
            ..Response::default()
        };

        assert_eq!(resp.response.body(), &expected_resp);
    }

    mod apq_body_preservation {
        use super::*;

        const APQ_TEST_QUERY: &str = "query MyOp($id: ID!) { thing(id: $id) { name } }";

        /// Spins up a mock subgraph server with APQ enabled and drives a single request through it.
        /// Each test supplies a `handle` function that acts as the subgraph server: it asserts on
        /// the received body and returns an HTTP response.
        async fn run_apq_test<Handler, Fut>(
            gql_body: graphql::Request,
            handle: Handler,
        ) -> SubgraphResponse
        where
            Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
            Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>>
                + Send
                + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket_addr = listener.local_addr().unwrap();
            let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
            tokio::task::spawn(serve(listener, handle));

            let subgraph_service = SubgraphService::new(
                "test",
                true,
                HttpClientServiceFactory::from_config(
                    "test",
                    &Configuration::default(),
                    ClientConfiguration::default(),
                ),
            )
            .expect("can create a SubgraphService");

            let query = gql_body.query.clone().unwrap_or_default();
            let subgraph_request = http::Request::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .uri(url)
                .body(gql_body)
                .unwrap();

            subgraph_service
                .oneshot(
                    SubgraphRequest::builder()
                        .supergraph_request(supergraph_request(&query))
                        .subgraph_request(subgraph_request)
                        .operation_kind(OperationKind::Query)
                        .subgraph_name(String::from("test"))
                        .context(Context::new())
                        .build(),
                )
                .await
                .unwrap()
        }

        // Verifies that operation_name, variables, and custom extensions are all forwarded
        // into the APQ body, not just the persistedQuery hash. The mock server handler
        // asserts on the received body directly, so the test fails if any field is missing.
        #[tokio::test(flavor = "multi_thread")]
        async fn test_apq_body_preserves_all_fields() {
            async fn handle(
                request: http::Request<Body>,
            ) -> Result<http::Response<Body>, Infallible> {
                let bytes = router::body::into_bytes(request.into_body())
                    .await
                    .expect("can read request body");
                let graphql_request: graphql::Request =
                    serde_json::from_reader(bytes.reader()).expect("valid graphql request");

                assert!(
                    graphql_request.query.is_none(),
                    "APQ body should omit query on first attempt"
                );
                assert_eq!(
                    graphql_request.operation_name.as_deref(),
                    Some("MyOp"),
                    "operation_name should be preserved in APQ body"
                );
                assert_eq!(
                    graphql_request.variables.get("id"),
                    Some(&serde_json_bytes::json!("42")),
                    "variables should be preserved in APQ body"
                );
                assert!(
                    graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                    "persistedQuery hash should be present"
                );
                assert!(
                    graphql_request.extensions.contains_key("myExt"),
                    "custom extensions should be preserved alongside persistedQuery"
                );

                let response = Response {
                    data: Some(Value::String(ByteString::from("test"))),
                    ..Response::default()
                };
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(serde_json::to_string(&response).unwrap().into())
                    .unwrap())
            }

            let gql_body = graphql::Request {
                query: Some(APQ_TEST_QUERY.to_string()),
                operation_name: Some("MyOp".to_string()),
                variables: serde_json_bytes::json!({"id": "42"})
                    .as_object()
                    .unwrap()
                    .clone(),
                extensions: serde_json_bytes::json!({"myExt": "value"})
                    .as_object()
                    .unwrap()
                    .clone(),
            };
            let response = run_apq_test(gql_body, handle).await.response.into_body();
            assert_eq!(response.data, Some(Value::String(ByteString::from("test"))));
            assert!(response.errors.is_empty());
        }

        // Verifies that on a PersistedQueryNotFound retry, the original query string,
        // operation name, and variables are all sent correctly. The mock server handler
        // distinguishes the first attempt (no query) from the retry (query present) and
        // asserts on the retry body fields directly.
        #[tokio::test(flavor = "multi_thread")]
        async fn test_apq_not_found_retry_preserves_original_body() {
            async fn handle(
                request: http::Request<Body>,
            ) -> Result<http::Response<Body>, Infallible> {
                let bytes = router::body::into_bytes(request.into_body())
                    .await
                    .expect("can read request body");
                let graphql_request: graphql::Request =
                    serde_json::from_reader(bytes.reader()).expect("valid graphql request");

                assert!(
                    graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                    "both attempts should include the persistedQuery hash"
                );

                if graphql_request.query.is_none() {
                    // First attempt: return PersistedQueryNotFound
                    let pqnf_response = Response {
                        errors: vec![
                            Error::builder()
                                .message(PERSISTED_QUERY_NOT_FOUND_MESSAGE)
                                .build(),
                        ],
                        ..Response::default()
                    };
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(serde_json::to_string(&pqnf_response).unwrap().into())
                        .unwrap());
                }

                // Second attempt: verify the retry body contains the original fields
                assert_eq!(
                    graphql_request.query.as_deref(),
                    Some(APQ_TEST_QUERY),
                    "retry should send the original query string"
                );
                assert_eq!(
                    graphql_request.operation_name.as_deref(),
                    Some("MyOp"),
                    "operation_name should be preserved on retry"
                );
                assert_eq!(
                    graphql_request.variables.get("id"),
                    Some(&serde_json_bytes::json!("42")),
                    "variables should be preserved on retry"
                );

                let success_response = Response {
                    data: Some(Value::String(ByteString::from("test"))),
                    ..Response::default()
                };
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(serde_json::to_string(&success_response).unwrap().into())
                    .unwrap())
            }

            let gql_body = graphql::Request {
                query: Some(APQ_TEST_QUERY.to_string()),
                operation_name: Some("MyOp".to_string()),
                variables: serde_json_bytes::json!({"id": "42"})
                    .as_object()
                    .unwrap()
                    .clone(),
                extensions: serde_json_bytes::json!({"myExt": "value"})
                    .as_object()
                    .unwrap()
                    .clone(),
            };
            let response = run_apq_test(gql_body, handle).await.response.into_body();
            assert_eq!(response.data, Some(Value::String(ByteString::from("test"))));
            assert!(response.errors.is_empty());
        }

        // Verifies that on a PersistedQueryNotSupported retry, the full original body is
        // restored: the query is present, the persistedQuery extension is removed, and any
        // other extensions are preserved. The mock server handler distinguishes the first
        // attempt (no query) from the retry (query present) and asserts on the retry body.
        #[tokio::test(flavor = "multi_thread")]
        async fn test_apq_not_supported_retry_restores_original_body() {
            async fn handle(
                request: http::Request<Body>,
            ) -> Result<http::Response<Body>, Infallible> {
                let bytes = router::body::into_bytes(request.into_body())
                    .await
                    .expect("can read request body");
                let graphql_request: graphql::Request =
                    serde_json::from_reader(bytes.reader()).expect("valid graphql request");

                if graphql_request.query.is_none() {
                    // First attempt: return PersistedQueryNotSupported
                    let pqns_response = Response {
                        errors: vec![
                            Error::builder()
                                .message(PERSISTED_QUERY_NOT_SUPPORTED_MESSAGE)
                                .build(),
                        ],
                        ..Response::default()
                    };
                    return Ok(http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(serde_json::to_string(&pqns_response).unwrap().into())
                        .unwrap());
                }

                // Second attempt: verify the retry body is the full original body
                assert_eq!(
                    graphql_request.query.as_deref(),
                    Some(APQ_TEST_QUERY),
                    "retry should send the original query string"
                );
                assert!(
                    !graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY),
                    "persistedQuery extension should be removed on retry"
                );
                assert!(
                    graphql_request.extensions.contains_key("myExt"),
                    "other extensions should be preserved on retry"
                );

                let success_response = Response {
                    data: Some(Value::String(ByteString::from("test"))),
                    ..Response::default()
                };
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(serde_json::to_string(&success_response).unwrap().into())
                    .unwrap())
            }

            let gql_body = graphql::Request {
                query: Some(APQ_TEST_QUERY.to_string()),
                operation_name: Some("MyOp".to_string()),
                variables: serde_json_bytes::json!({"id": "42"})
                    .as_object()
                    .unwrap()
                    .clone(),
                extensions: serde_json_bytes::json!({"myExt": "value"})
                    .as_object()
                    .unwrap()
                    .clone(),
            };
            let response = run_apq_test(gql_body, handle).await.response.into_body();
            assert_eq!(response.data, Some(Value::String(ByteString::from("test"))));
            assert!(response.errors.is_empty());
        }
    }

    #[test]
    fn it_gets_uri_details() {
        let path = "https://example.com/path".parse().unwrap();
        let (host, port, path) = super::get_uri_details(&path);

        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/path");
    }

    #[test]
    fn it_converts_ok_http_to_graphql() {
        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(None)
            .unwrap()
            .into_parts();
        let actual = super::http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder().build();
        assert_eq!(actual, expected);
    }

    #[test]
    fn it_converts_error_http_to_graphql() {
        let (parts, body) = http::Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .body(None)
            .unwrap()
            .into_parts();
        let actual = super::http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .error(
                super::FetchError::SubrequestHttpError {
                    status_code: Some(418),
                    service: "test_service".into(),
                    reason: "418: I'm a teapot".into(),
                }
                .to_graphql_error(None),
            )
            .build();
        assert_response_eq_ignoring_error_id!(actual, expected);
    }

    #[test]
    fn it_converts_http_with_body_to_graphql() {
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value"
            }
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = super::http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .build();
        assert_eq!(actual, expected);
    }

    #[test]
    fn it_converts_http_with_graphql_errors_to_graphql() {
        let error = graphql::Error::builder()
            .message("error was encountered for test")
            .extension_code("SOME_EXTENSION")
            .extension("service", "test_service")
            .build();
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value",
                "error_field": null,
            },
            "errors": [error],
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::OK)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = super::http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .error(error)
            .build();
        assert_response_eq_ignoring_error_id!(actual, expected);
    }

    #[test]
    fn it_converts_error_http_with_graphql_errors_to_graphql() {
        let error = graphql::Error::builder()
            .message("error was encountered for test")
            .extension_code("SOME_EXTENSION")
            .extension("service", "test_service")
            .build();
        let mut json = serde_json::json!({
            "data": {
                "some_field": "some_value",
                "error_field": null,
            },
            "errors": [error],
        });

        let (parts, body) = http::Response::builder()
            .status(StatusCode::IM_A_TEAPOT)
            .body(Some(Ok(Bytes::from(json.to_string()))))
            .unwrap()
            .into_parts();

        let actual = super::http_response_to_graphql_response(
            "test_service",
            Ok(ContentType::ApplicationGraphqlResponseJson),
            body,
            &parts,
        );

        let expected = graphql::Response::builder()
            .data(json["data"].take())
            .error(
                super::FetchError::SubrequestHttpError {
                    status_code: Some(418),
                    service: "test_service".into(),
                    reason: "418: I'm a teapot".into(),
                }
                .to_graphql_error(None),
            )
            .error(error)
            .build();
        assert_response_eq_ignoring_error_id!(expected, actual);
    }
}
