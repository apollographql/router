//! Tower fetcher for subgraphs.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::task::Poll;

use bytes::Bytes;
use futures::TryFutureExt;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Request;
use http::StatusCode;
use http::response::Parts;
use http_body::Body;
use http_body_util::LengthLimitError;
use hyper_rustls::ConfigBuilderExt;
use itertools::Itertools;
use opentelemetry::Key;
use opentelemetry::KeyValue;
use rustls::RootCertStore;
use tokio::sync::oneshot;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::Instrument;
use tracing::instrument;

use super::Plugins;
use super::http::HttpRequest;
use super::layers::content_negotiation::ContentType;
use super::layers::content_negotiation::SubgraphLayer;
use super::layers::content_negotiation::get_graphql_content_type;
use super::layers::content_negotiation::http_response_to_graphql_response;
use super::router::body::RouterBody;
use super::subgraph::SubgraphRequestId;
use crate::Context;
use crate::Notify;
use crate::batching::BatchQuery;
use crate::batching::BatchQueryInfo;
use crate::batching::SubgraphBatchRequest;
use crate::batching::assemble_batch;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::configuration::SubgraphApq;
use crate::configuration::TlsClientAuth;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::error::FetchError;
use crate::error::SubgraphBatchingError;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::unconstrained_buffer::UnconstrainedBuffer;
use crate::layers::unconstrained_buffer::UnconstrainedBufferLayer;
use crate::plugins::limits::SubgraphResponseSizeLimit;
use crate::plugins::subscription::SubscriptionConfig;
use crate::plugins::subscription::subgraph::SubscriptionSubgraphLayer;
use crate::plugins::telemetry::config_new::events::log_event;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventRequest;
use crate::plugins::telemetry::config_new::subgraph::events::SubgraphEventResponse;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphRequestBodySize;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphResponseBodySize;
use crate::plugins::telemetry::consts::SUBGRAPH_REQUEST_SPAN_NAME;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::http::service::WireByteCount;
use crate::services::layers::apq::subgraph::SubgraphApqLayer;
use crate::services::router;
use crate::services::subgraph;

#[allow(clippy::declare_interior_mutable_const)]
pub(crate) static APPLICATION_JSON_HEADER_VALUE: HeaderValue =
    HeaderValue::from_static("application/json");

/// Client for interacting with subgraphs.
#[derive(Clone)]
pub(crate) struct SubgraphService {
    /// Pre-built HTTP client service with all plugin layers already folded in.
    /// Used on the hot (non-batching) path to avoid re-folding plugins per request.
    http_client: crate::services::http::BoxCloneService,
    service: Arc<String>,
}

impl SubgraphService {
    pub(crate) fn new(
        service: impl Into<String>,
        http_client: crate::services::http::BoxCloneService,
    ) -> Result<Self, BoxError> {
        let name = service.into();
        Ok(Self {
            http_client,
            service: Arc::new(name),
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

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http_client.poll_ready(cx)
    }

    fn call(&mut self, request: SubgraphRequest) -> Self::Future {
        let service_name = self.service.clone();

        let fresh_client = self.http_client.clone();
        let http_client = std::mem::replace(&mut self.http_client, fresh_client);

        Box::pin(async move { call_http(request, http_client, &service_name).await })
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

/// Process a single subgraph batch request
#[instrument(skip(http_client, contexts, request))]
pub(crate) async fn process_batch(
    http_client: crate::services::http::BoxCloneService,
    service: String,
    mut contexts: Vec<(Context, SubgraphRequestId)>,
    request: http::Request<RouterBody>,
    listener_count: usize,
) -> Result<Vec<SubgraphResponse>, FetchError> {
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
    let service_name = service.to_string();

    // Update our batching metrics (just before we fetch)
    u64_histogram!(
        "apollo.router.operations.batching.size",
        "Number of queries contained within each query batch",
        listener_count as u64,
        mode = BatchingMode::BatchHttpLink.to_string(), // Only supported mode right now
        subgraph = service_name.clone()
    );

    u64_counter!(
        "apollo.router.operations.batching",
        "Total requests with batched operations",
        1,
        // XXX(@goto-bus-stop): Should these be `batching.mode`, `batching.subgraph`?
        // Also, other metrics use a different convention to report the subgraph name
        mode = BatchingMode::BatchHttpLink.to_string(), // Only supported mode right now
        subgraph = service_name.clone()
    );

    // Perform the actual fetch. If this fails then we didn't manage to make the call at all, so we can't do anything with it.
    tracing::debug!("fetching from subgraph: {service}");
    let (parts, content_type, body) = match do_fetch(http_client, &batch_context, &service, request)
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
                    service: service_name.clone(),
                    reason: format!("cannot create the http response from error: {err:?}"),
                })?;
            let (parts, body) = resp.into_parts();
            let body =
                serde_json::to_vec(&body).map_err(|err| FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.clone(),
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
            opentelemetry::Value::String(service_name.clone().into()),
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
            service: service_name.clone(),
            reason: "no body in response".to_string(),
        })??)
        .map_err(|error| FetchError::SubrequestMalformedResponse {
            service: service_name.clone(),
            reason: error.to_string(),
        })?;

    tracing::debug!("json value from body is: {value:?}");

    let array = ensure_array!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
        service: service_name.clone(),
        reason: error.to_string(),
    })?;
    let mut graphql_responses = Vec::with_capacity(array.len());
    for value in array {
        let object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.clone(),
                reason: error.to_string(),
            })?;

        // Map our Vec<u8> into Bytes
        // Map our serde conversion error to a FetchError
        let body = Some(
            serde_json::to_vec(&object)
                .map(|v| v.into())
                .map_err(|error| FetchError::SubrequestMalformedResponse {
                    service: service_name.clone(),
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
            service: service_name.clone(),
            reason: format!(
                "number of contexts ({}) is not equal to number of graphql responses ({})",
                contexts.len(),
                graphql_responses.len()
            ),
        });
    }

    // We are going to pop contexts from the back, so let's reverse our contexts
    contexts.reverse();
    // Build an http Response for each graphql response
    let subgraph_responses: Result<Vec<_>, _> = graphql_responses
        .into_iter()
        .map(|res| {
            http::Response::builder()
                .status(parts.status)
                .version(parts.version)
                .body(res)
                .map(|mut http_res| {
                    *http_res.headers_mut() = parts.headers.clone();
                    // Use the original context for the request to create the response
                    let (context, id) =
                        contexts.pop().expect("we have a context for each response");
                    let resp = SubgraphResponse::new_from_response(
                        http_res,
                        context,
                        service_name.clone(),
                        id,
                    );

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
async fn notify_batch_query(
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

struct BatchInfo {
    service: String,
    /// A pre-readied HTTP client service for this subgraph.
    http_client: crate::services::http::BoxCloneService,
    request: http::Request<RouterBody>,
    contexts: Vec<(Context, SubgraphRequestId)>,
}

type BatchResult = (
    BatchInfo,
    Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
);

/// Collect all batch requests and process them concurrently
///
/// # Panics
/// The HTTP client services inside the svc_map must already be readied: otherwise, it may panic.
#[instrument(skip_all)]
pub(crate) async fn process_batches(
    svc_map: HashMap<String, Vec<BatchQueryInfo>>,
) -> Result<(), BoxError> {
    // We need to strip out the senders so that we can work with them separately.
    let mut errors = vec![];
    let (info, txs): (Vec<_>, Vec<_>) =
        futures::future::join_all(svc_map.into_iter().map(|(service, requests)| async {
            let SubgraphBatchRequest {
                http_client,
                contexts,
                request,
                txs,
            } = assemble_batch(requests).await?;

            Ok((
                BatchInfo {
                    service,
                    http_client,
                    request,
                    contexts,
                },
                txs,
            ))
        }))
        .await
        .into_iter()
        .filter_map(|x: Result<BatchResult, BoxError>| x.map_err(|e| errors.push(e)).ok())
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
    // It is not ok to panic if the length of the txs and info do not match. Let's make sure they
    // do
    if txs.len() != info.len() {
        return Err(SubgraphBatchingError::ProcessingFailed(
            "length of txs and info are not equal".to_string(),
        )
        .into());
    }
    let batch_futures = info.into_iter().zip_eq(txs).map(
        |(
            BatchInfo {
                service,
                http_client,
                request,
                contexts,
            },
            senders,
        )| async move {
            let listener_count = senders.len();
            let batch_result = process_batch(
                http_client,
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
    http_client: crate::services::http::BoxCloneService,
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
        let response_rx = query.signal_progress(http_client, request).await?;

        // Park this query until we have our response and pass it back up
        response_rx
            .await
            .map_err(|err| FetchError::SubrequestBatchingError {
                service: service_name.to_string(),
                reason: format!("tx receive failed: {err}"),
            })?
    } else {
        tracing::debug!("we called http");
        call_single_http(request, http_client, service_name).await
    }
}

/// call_single_http makes http calls with modified graphql::Request (body)
async fn call_single_http(
    request: SubgraphRequest,
    client: crate::services::http::BoxCloneService,
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
    let request = http::Request::from_parts(parts, router::body::from_bytes(body));

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

    if let Some(level) = log_request_level {
        let mut attrs = Vec::with_capacity(5);
        let headers_str = crate::services::header_masking::masked_headers_for_log(
            &context,
            crate::services::header_masking::Direction::Request,
            Some(service_name),
            request.headers(),
        );
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.headers"),
            opentelemetry::Value::String(headers_str.into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.method"),
            opentelemetry::Value::String(format!("{}", request.method()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.version"),
            opentelemetry::Value::String(format!("{:?}", request.version()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("http.request.body"),
            opentelemetry::Value::String(format!("{:?}", request.body()).into()),
        ));
        attrs.push(KeyValue::new(
            Key::from_static_str("subgraph.name"),
            opentelemetry::Value::String(service_name.to_string().into()),
        ));

        log_event(
            level,
            "subgraph.request",
            attrs,
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

async fn do_fetch(
    mut client: crate::services::http::BoxCloneService,
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
        services: Vec<(String, subgraph::BoxCloneService)>,
        plugins: Arc<Plugins>,
        notify: Notify<String, graphql::Response>,
        subscription_config: Option<Arc<SubscriptionConfig>>,
        apq_config: SubgraphConfiguration<SubgraphApq>,
    ) -> Self {
        let mut map = HashMap::with_capacity(services.len());
        for (name, service) in services.into_iter() {
            // We have to do a little dance here to insert the subscription and APQ layers at the
            // right place: *after* all user plugins, but *before* the subgraph service proper.
            // SubgraphLayer sits closest to the inner SubgraphService so that per-request
            // Accept/Content-Type headers are injected right before HTTP dispatch, and so that
            // APQ retries (which re-call the inner service) still go out with those headers set.
            let apq_enabled = apq_config.get(&name).enabled;

            let inner_service = ServiceBuilder::new()
                .layer(SubscriptionSubgraphLayer::new(
                    notify.clone(),
                    subscription_config.clone(),
                    Arc::from(name.clone()),
                ))
                .layer(SubgraphApqLayer::new(apq_enabled))
                .layer(SubgraphLayer::default())
                .service(service)
                .boxed_clone();

            // One buffer per named subgraph provides per-subgraph backpressure and is
            // required for correct LoadShed / RateLimit behaviour from traffic-shaping
            // plugins (see ServiceBuilderExt::buffered).
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

    pub(crate) fn create(&self, name: &str) -> Option<subgraph::BoxCloneService> {
        self.services.get(name).map(|svc| svc.clone().boxed_clone())
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use SubgraphRequest;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::Message;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use bytes::Buf;
    use futures::StreamExt;
    use http::StatusCode;
    use http::Uri;
    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http::header::HOST;
    use mime::APPLICATION_JSON;
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
    use crate::protocols::websocket::ServerMessage;
    use crate::protocols::websocket::WebSocketProtocol;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::http::HttpClientServiceFactory;
    use crate::services::layers::apq::subgraph::SubgraphApqService;
    use crate::services::layers::content_negotiation::ContentType;
    use crate::services::layers::content_negotiation::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
    use crate::services::layers::content_negotiation::SubgraphContentNegotiationService;
    use crate::services::layers::content_negotiation::http_response_to_graphql_response;
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

    /// Wraps a bare `SubgraphService` with `SubgraphLayer`, mirroring the position it occupies in
    /// `SubgraphServiceFactory::new`. Most unit tests construct a `SubgraphService` directly and
    /// call it without going through the factory, which would otherwise skip Accept/Content-Type
    /// header injection entirely.
    fn with_content_negotiation_layer(
        s: SubgraphService,
    ) -> SubgraphContentNegotiationService<SubgraphService> {
        SubgraphLayer::default().layer(s)
    }

    /// Manually rebuilds the production layer stack (Subscription -> APQ -> SubgraphLayer ->
    /// SubgraphService) for subscriptions tests, which construct a `SubgraphService` directly
    /// instead of going through the `SubgraphServiceFactory`.
    fn with_subscription_layer(
        s: SubgraphService,
    ) -> SubscriptionSubgraphService<
        SubgraphApqService<SubgraphContentNegotiationService<SubgraphService>>,
    > {
        let service_name = Arc::from(s.service.to_string());
        let apq_service = SubgraphApqLayer::new(false).layer(with_content_negotiation_layer(s));
        SubscriptionSubgraphLayer::new(
            Notify::builder().build(),
            Some(Arc::new(subscription_config())),
            service_name,
        )
        .layer(apq_service)
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
            SubgraphService::new("testbis", HttpClientServiceFactory::for_test("testbis"))
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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: HTTP fetch failed: connection closed before message completed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_invalid_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_ok_status_invalid_response(listener));
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_response_size_limit_exceeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_large_response(listener));
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "response was malformed: invalid"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_invalid_status_invalid_response_application_graphql() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(
            emulate_subgraph_invalid_response_invalid_status_application_graphql(listener),
        );
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: 401: Unauthorized"
        );
        assert_eq!(
            response.response.body().errors[1].message,
            "response was malformed: expected value at line 1 column 1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let spawned_task = tokio::task::spawn(emulate_correct_websocket_server(listener));
        let subgraph_service = with_subscription_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
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
                SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
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
            assert!(err_str.starts_with("Websocket fetch failed: cannot connect websocket to subgraph: WebSocket upgrade failed. Status: 400 Bad Request; Headers: [\"content-type\": \"text/plain; charset=utf-8\"; \"content-length\": \"11\";"));

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
                SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bad_status_code_should_not_fail() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_bad_request(listener));
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: 400: Bad Request"
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

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: subgraph response does not contain 'content-type' header; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_invalid_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_invalid_content_type(listener));

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: subgraph response contains invalid 'content-type' header value \"application/json,application/json\"; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unsupported_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unsupported_content_type(listener));

        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: subgraph response contains unsupported content-type: text/html; expected content-type: application/json or content-type: application/graphql-response+json"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unauthorized() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_unauthorized(listener));
        let subgraph_service = with_content_negotiation_layer(
            SubgraphService::new("test", HttpClientServiceFactory::for_test("test"))
                .expect("can create a SubgraphService"),
        );

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
            "HTTP fetch failed: 401: Unauthorized"
        );
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
        let actual = http_response_to_graphql_response(
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
        let actual = http_response_to_graphql_response(
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

        let actual = http_response_to_graphql_response(
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

        let actual = http_response_to_graphql_response(
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

        let actual = http_response_to_graphql_response(
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
