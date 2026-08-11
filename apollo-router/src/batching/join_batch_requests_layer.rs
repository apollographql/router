use std::collections::HashMap;
use std::sync::Arc;

use bytes::BytesMut;
use futures::future::BoxFuture;
use http::StatusCode;
use http_body_util::BodyExt as _;
use itertools::Itertools;
use tokio::sync::oneshot;
use tower::BoxError;
use tower::Service as _;
use tower::ServiceExt as _;
use tracing::instrument;

use crate::Context;
use crate::batching::BatchQuery;
use crate::batching::BatchQueryInfo;
use crate::batching::get_request_id;
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::error::FetchError;
use crate::error::SubgraphBatchingError;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::subgraph::SubgraphRequestId;

/// Analyze the query plan for a batch query.
#[derive(Clone)]
pub(crate) struct JoinBatchRequestsLayer {
    // TODO(@goto-bus-stop): Should take this from the request instead.
    subgraph_name: Arc<str>,
}

impl JoinBatchRequestsLayer {
    pub(crate) fn new(subgraph_name: impl Into<Arc<str>>) -> Self {
        Self {
            subgraph_name: subgraph_name.into(),
        }
    }
}

impl<S> tower::Layer<S> for JoinBatchRequestsLayer {
    type Service = JoinBatchRequestsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JoinBatchRequestsService {
            inner,
            subgraph_name: self.subgraph_name.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct JoinBatchRequestsService<S> {
    inner: S,
    subgraph_name: Arc<str>,
}

impl<S> tower::Service<HttpRequest> for JoinBatchRequestsService<S>
where
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let subgraph_name = self.subgraph_name.clone();

        Box::pin(async move {
            // We use configuration to determine if calls may be batched. If we have Batching
            // configuration, then we check (batch_include()) if the current subgraph has batching enabled
            // in configuration. If it does, we then start to process a potential batch.
            //
            // If we are processing a batch, then we'd like to park tasks here, but we can't park them whilst
            // we have the context extensions lock held. That would be very bad...
            // We grab the (potential) BatchQuery and then operate on it later
            let opt_batch_query = req.context.extensions().with_lock(|lock| {
                lock.get::<Batching>()
                    .and_then(|batching_config| {
                        // TODO(@goto-bus-stop): Instead we could use `.option_layer`
                        batching_config.batch_include(&subgraph_name).then_some(())
                    })
                    .and_then(|_| lock.get::<BatchQuery>().cloned())
                    .and_then(|bq| (!bq.finished()).then_some(bq))
            });

            // If we have a batch query, then it's time for batching
            if let Some(query) = opt_batch_query {
                let response_rx = query
                    .signal_progress(
                        subgraph_name.clone(),
                        // FIXME(@goto-bus-stop): temporary to make the types work
                        inner.boxed_clone(),
                        req,
                    )
                    .await?;

                // Park this query until we have our response and pass it back up
                response_rx
                    .await
                    .map_err(|err| FetchError::SubrequestBatchingError {
                        service: subgraph_name.to_string(),
                        reason: format!("tx receive failed: {err}"),
                    })?
            } else {
                inner.call(req).await
            }
        })
    }
}

/// A batch of requests that we'll send to a subgraph (...as a single batch request).
struct SubgraphBatchRequest {
    http_client: crate::services::http::BoxCloneService,
    contexts: Vec<(Context, SubgraphRequestId)>,
    request: http::Request<RouterBody>,
    txs: Vec<oneshot::Sender<Result<HttpResponse, BoxError>>>,
}

// Assemble a single batch request to a subgraph
async fn assemble_batch(
    batch_queries: Vec<BatchQueryInfo>,
) -> Result<SubgraphBatchRequest, BoxError> {
    let mut txs = Vec::with_capacity(batch_queries.len());
    let mut contexts = Vec::with_capacity(batch_queries.len());
    let mut body_streams = Vec::with_capacity(batch_queries.len());

    let mut iter = batch_queries.into_iter();

    let first = iter.next().ok_or(SubgraphBatchingError::RequestsIsEmpty)?;
    let http_client = first.http_client;
    txs.push(first.sender);
    let request_id = get_request_id(&first.request);
    contexts.push((first.request.context, request_id));
    // We'll use the HTTP parts (headers, URI etc) from the first request for the whole batch
    let (parts, first_body) = first.request.http_request.into_parts();
    body_streams.push(first_body);

    for batch_query in iter {
        txs.push(batch_query.sender);
        let request_id = get_request_id(&batch_query.request);
        contexts.push((batch_query.request.context, request_id));
        body_streams.push(batch_query.request.http_request.into_body());
    }
    debug_assert_eq!(txs.len(), contexts.len());
    debug_assert_eq!(txs.len(), body_streams.len());

    // Construct the actual byte body of the batched request
    let mut batch_body = BytesMut::new();
    batch_body.extend_from_slice(&[b'[']);
    for body in body_streams {
        let bytes = body.collect().await.unwrap().to_bytes();
        batch_body.extend_from_slice(&bytes);
        batch_body.extend_from_slice(&[b',']);
    }
    // There's guaranteed to be a comma here, because `body_streams` is guaranteed to be non-empty,
    // because we'd have returned with a RequestsIsEmpty error otherwise.
    *batch_body.last_mut().unwrap() = b']';
    let batch_body = batch_body.freeze();

    // Generate the final request and pass it up
    let request = http::Request::from_parts(parts, router::body::from_bytes(batch_body));
    Ok(SubgraphBatchRequest {
        http_client,
        contexts,
        request,
        txs,
    })
}

/// Process a single subgraph batch request
#[instrument(skip(http_client, contexts, request))]
async fn process_batch(
    mut http_client: crate::services::http::BoxCloneService,
    service: &str,
    mut contexts: Vec<(Context, SubgraphRequestId)>,
    request: http::Request<RouterBody>,
    listener_count: usize,
) -> Result<Vec<HttpResponse>, FetchError> {
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
    let (parts, body) = match http_client
        .call(HttpRequest {
            http_request: request,
            context: batch_context.clone(),
        })
        .await
    {
        Ok(response) => {
            let (parts, response_body) = response.http_response.into_parts();
            let body = router::body::into_bytes(response_body)
                .await
                .map_err(|err| FetchError::SubrequestHttpError {
                    status_code: Some(parts.status.as_u16()),
                    service: service_name.clone(),
                    reason: err.to_string(),
                })?;
            (parts, body)
        }
        Err(err) => {
            let err = FetchError::SubrequestHttpError {
                status_code: None,
                service: service_name.clone(),
                reason: err.to_string(),
            };
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
            (parts, body.into())
        }
    };

    // Mask sensitive response headers once, for reuse in both the telemetry
    // event and the debug log below. Logging the raw `parts` would otherwise
    // leak the very header values this masking redacts.
    let headers_str = crate::services::header_masking::masked_headers_for_log(
        &batch_context,
        crate::services::header_masking::Direction::Response,
        Some(service),
        &parts.headers,
    );

    tracing::debug!(
        "parts status: {:?}, version: {:?}, headers: {headers_str}, body: {body:?}",
        parts.status,
        parts.version,
    );
    let value =
        serde_json::from_slice(&body).map_err(|error| FetchError::SubrequestMalformedResponse {
            service: service_name.clone(),
            reason: error.to_string(),
        })?;

    tracing::debug!("json value from body is: {value:?}");

    let array = ensure_array!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
        service: service_name.clone(),
        reason: error.to_string(),
    })?;
    let mut exploded_bodies = Vec::with_capacity(array.len());
    for value in array {
        let object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.clone(),
                reason: error.to_string(),
            })?;

        // Map our Vec<u8> into Bytes
        // Map our serde conversion error to a FetchError
        let body = serde_json::to_vec(&object).map_err(|error| {
            FetchError::SubrequestMalformedResponse {
                service: service_name.clone(),
                reason: error.to_string(),
            }
        })?;

        exploded_bodies.push(router::body::from_bytes(body));
    }

    tracing::debug!("we have a vec of graphql_responses: {exploded_bodies:?}");
    // Before we process our graphql responses, ensure that we have a context for each
    // response
    if exploded_bodies.len() != contexts.len() {
        return Err(FetchError::SubrequestBatchingError {
            service: service_name.clone(),
            reason: format!(
                "number of contexts ({}) is not equal to number of graphql responses ({})",
                contexts.len(),
                exploded_bodies.len()
            ),
        });
    }

    // We are going to pop contexts from the back, so let's reverse our contexts
    contexts.reverse();
    // Build an http Response for each graphql response
    let exploded_responses: Result<Vec<_>, _> = exploded_bodies
        .into_iter()
        .map(|body| {
            http::Response::builder()
                .status(parts.status)
                .version(parts.version)
                .body(body)
                .map(|mut http_response| {
                    *http_response.headers_mut() = parts.headers.clone();
                    // Use the original context for the request to create the response
                    let (context, _id) =
                        contexts.pop().expect("we have a context for each response");
                    let resp = HttpResponse {
                        http_response,
                        context,
                    };

                    // Avoid `{resp:?}`: SubgraphResponse's derived Debug prints
                    // the response HeaderMap unmasked. Log the non-header parts.
                    tracing::debug!(
                        "built subgraph response for {service}: status={:?}, body={:?}",
                        resp.http_response.status(),
                        resp.http_response.body(),
                    );
                    resp
                })
                .map_err(|e| FetchError::MalformedResponse {
                    reason: e.to_string(),
                })
        })
        .collect();

    match &exploded_responses {
        Ok(responses) => tracing::debug!("built {} subgraph responses", responses.len()),
        Err(error) => tracing::debug!("failed to build subgraph responses: {error}"),
    }
    exploded_responses
}

/// Notify all listeners of a batch query of the results
async fn notify_batch_query(
    service: String,
    senders: Vec<oneshot::Sender<Result<HttpResponse, BoxError>>>,
    responses: Result<Vec<HttpResponse>, FetchError>,
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
                if let Err(log_error) = tx.send(Err(Box::new(e.clone()))).map_err(|_| {
                    FetchError::SubrequestBatchingError {
                        service: service.clone(),
                        reason: format!("tx send failed: {e:?}"),
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
    Vec<oneshot::Sender<Result<HttpResponse, BoxError>>>,
);

/// Collect all batch requests and process them concurrently
///
/// # Panics
/// The HTTP client services inside the svc_map must already be readied: otherwise, it may panic.
#[instrument(skip_all)]
pub(super) async fn process_batches(
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
            let batch_result =
                process_batch(http_client, &service, contexts, request, listener_count).await;

            notify_batch_query(service, senders, batch_result).await
        },
    );

    futures::future::try_join_all(batch_futures).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::SubgraphBatchRequest;
    use super::assemble_batch;
    use crate::Context;
    use crate::batching::BatchQueryInfo;
    use crate::graphql;
    use crate::services::http::HttpClientServiceFactory;
    use crate::services::http::HttpRequest;
    use crate::services::http::HttpResponse;
    use crate::services::router::body;
    use crate::services::subgraph::SubgraphRequestId;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_assembles_batch() {
        // Assemble a list of requests for testing
        // TODO(@goto-bus-stop): use the layer instead of assemble_batch directly?
        let (receivers, requests): (Vec<_>, Vec<_>) = (0..2)
            .map(|index| {
                let subgraph_name = Arc::from("test");
                let (tx, rx) = oneshot::channel();
                let gql_request = graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build();

                let body = body::from_bytes(serde_json::to_vec(&gql_request).unwrap());
                let context = Context::new();
                context.extensions().with_lock(|lock| {
                    lock.insert(SubgraphRequestId::new());
                });

                (
                    rx,
                    BatchQueryInfo {
                        http_client: HttpClientServiceFactory::for_test(&subgraph_name),
                        request: HttpRequest {
                            http_request: http::Request::builder().body(body).unwrap(),
                            context,
                        },
                        sender: tx,
                        subgraph_name,
                    },
                )
            })
            .unzip();

        // Create a vector of the input request context IDs for comparison
        let input_context_ids = requests
            .iter()
            .map(|r| r.request.context.id.clone())
            .collect::<Vec<String>>();
        // Assemble them
        let SubgraphBatchRequest {
            http_client: _,
            contexts,
            request,
            txs,
        } = assemble_batch(requests)
            .await
            .expect("it can assemble a batch");

        let output_context_ids = contexts
            .iter()
            .map(|r| r.0.id.clone())
            .collect::<Vec<String>>();
        // Make sure all of our contexts are preserved during assembly
        assert_eq!(input_context_ids, output_context_ids);

        // We should see the aggregation of all of the requests
        let actual: Vec<graphql::Request> = serde_json::from_str(
            std::str::from_utf8(&body::into_bytes(request.into_body()).await.unwrap()).unwrap(),
        )
        .unwrap();

        let expected: Vec<_> = (0..2)
            .map(|index| {
                graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build()
            })
            .collect();
        assert_eq!(actual, expected);

        // We should also have all of the correct senders and they should be linked to the correct waiter
        // Note: We reverse the senders since they should be in reverse order when assembled
        assert_eq!(txs.len(), receivers.len());
        for (index, (tx, rx)) in Iterator::zip(txs.into_iter(), receivers).enumerate() {
            let data = serde_json_bytes::json!({
                "data": {
                    format!("slot{index}"): "valid"
                }
            });
            let graphql_response = graphql::Response::builder().data(data.clone()).build();
            let body = body::from_bytes(serde_json::to_vec(&graphql_response).unwrap());

            let response = HttpResponse {
                http_response: http::Response::builder()
                    .header(
                        http::header::CONTENT_TYPE,
                        "application/graphql-response+json",
                    )
                    .body(body)
                    .unwrap(),
                context: Context::new(),
            };

            assert!(tx.send(Ok(response)).is_ok());

            // We want to make sure that we don't hang the test if we don't get the correct message
            let received = tokio::time::timeout(Duration::from_millis(10), rx)
                .await
                .unwrap()
                .unwrap()
                .unwrap();

            let body = body::into_bytes(received.http_response.into_body())
                .await
                .unwrap();
            let body: graphql::Response = serde_json::from_slice(&body).unwrap();
            assert_eq!(body.data, Some(data));
        }
    }
}
