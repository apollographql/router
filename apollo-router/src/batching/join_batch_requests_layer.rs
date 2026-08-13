use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
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
use crate::configuration::Batching;
use crate::configuration::BatchingMode;
use crate::error::FetchError;
use crate::error::SubgraphBatchingError;
use crate::graphql;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::router::body::RouterBody;

/// Intercept requests that are part of a batch, and batch them up together into a single request
/// per subgraph.
///
/// All subgraph requests for a batch, even for different subgraphs, are sent at the same time once
/// the whole batch is ready.
//
// This layer works together with the `crate::batching::Batch` structure, which calls back into the
// `process_batches` function declared in this file, and that's where eventually the inner service
// is called.
#[derive(Clone)]
pub(crate) struct JoinBatchRequestsLayer {
    subgraph_name: Arc<str>,
}

impl JoinBatchRequestsLayer {
    /// Create a layer that joins batch requests into a single request per subgraph.
    ///
    /// The subgraph name must be passed in so requests for the same subgraph can be identified.
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
                        // Send our type-erased inner service to the batching worker.
                        // Only one of the inner services will actually be called
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
    contexts: Vec<Context>,
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
    contexts.push(first.request.context);
    // We'll use the HTTP parts (headers, URI etc) from the first request for the whole batch
    let (parts, first_body) = first.request.http_request.into_parts();
    body_streams.push(first_body);

    for batch_query in iter {
        txs.push(batch_query.sender);
        contexts.push(batch_query.request.context);
        body_streams.push(batch_query.request.http_request.into_body());
    }
    debug_assert_eq!(txs.len(), contexts.len());
    debug_assert_eq!(txs.len(), body_streams.len());

    // Construct the actual byte body of the batched request
    let batch_body = {
        let mut batch_requests = Vec::with_capacity(body_streams.len());
        for body in body_streams {
            let bytes = body
                .collect()
                .await
                .map_err(|err| SubgraphBatchingError::ProcessingFailed(err.to_string()))?
                .to_bytes();
            let request: graphql::Request = serde_json::from_slice(&bytes)
                .map_err(|err| SubgraphBatchingError::ProcessingFailed(err.to_string()))?;
            batch_requests.push(request);
        }

        Bytes::from(
            serde_json::to_vec(&batch_requests)
                .map_err(|err| SubgraphBatchingError::ProcessingFailed(err.to_string()))?,
        )
    };

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
    contexts: Vec<Context>,
    request: http::Request<RouterBody>,
    listener_count: usize,
) -> Result<Vec<HttpResponse>, FetchError> {
    // We need a "representative context" for a batch. We use the first context in our list of
    // contexts
    let batch_context = contexts
        .first()
        .expect("we have at least one context in the batch")
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

    // Build an http Response for each graphql response
    let exploded_responses = exploded_bodies
        .into_iter()
        // We already checked the lengths are equal
        .zip_eq(contexts)
        .map(|(body, context)| {
            http::Response::builder()
                .status(parts.status)
                .version(parts.version)
                .body(body)
                .map(|mut http_response| {
                    *http_response.headers_mut() = parts.headers.clone();
                    // Use the original context for the request to create the response
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
        .collect::<Result<Vec<_>, _>>();

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
    contexts: Vec<Context>,
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use futures::future::join_all;
    use rstest::rstest;
    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;

    use super::JoinBatchRequestsLayer;
    use crate::Context;
    use crate::batching::Batch;
    use crate::configuration::Batching;
    use crate::configuration::CommonBatchingConfig;
    use crate::configuration::subgraph::SubgraphConfiguration;
    use crate::graphql;
    use crate::services::http::HttpRequest;
    use crate::services::http::HttpResponse;
    use crate::services::router::body;
    use crate::spec::QueryHash;

    fn graphql_request(query: &str, context: Context) -> HttpRequest {
        let gql_request = graphql::Request::fake_builder().query(query).build();
        let body = body::from_bytes(serde_json::to_vec(&gql_request).unwrap());
        HttpRequest {
            http_request: http::Request::builder().body(body).unwrap(),
            context,
        }
    }

    /// Set up all the required context for a subgraph batch query
    async fn context_for_batch_index(batch: &Arc<Batch>, index: usize) -> Context {
        // This gives us the same query hash every time, but as long as we only use one query each
        // time, that's okay. The hashes identify nodes in a query plan, not individual requests in
        // the batch.
        let query_hash = Arc::new(QueryHash::default());
        let query = Batch::query_for_index(batch.clone(), index).unwrap();
        query
            .set_query_hashes(vec![query_hash.clone()])
            .await
            .unwrap();

        // Subgraph batching on for all subgraphs
        let config = Batching {
            enabled: true,
            subgraph: Some(SubgraphConfiguration {
                all: CommonBatchingConfig { enabled: true },
                subgraphs: HashMap::new(),
            }),
            ..Default::default()
        };

        let context = Context::new();
        context.extensions().with_lock(|lock| {
            lock.insert(config);
            lock.insert(query);
            lock.insert(query_hash);
        });
        context
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_passes_through_unbatched_requests() {
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();

        let driver = tokio::task::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            let body = body::into_bytes(request.http_request.into_body())
                .await
                .unwrap();
            let gql_request: graphql::Request = serde_json::from_slice(&body).unwrap();
            assert_eq!(gql_request.query.as_deref(), Some("{ passthrough }"));

            let data = serde_json_bytes::json!({ "passthrough": true });
            let response_body = body::from_bytes(
                serde_json::to_vec(&graphql::Response::builder().data(data).build()).unwrap(),
            );
            responder.send_response(HttpResponse {
                http_response: http::Response::builder().body(response_body).unwrap(),
                context: request.context,
            });
        });

        let mut service = ServiceBuilder::new()
            .layer(JoinBatchRequestsLayer::new("test_subgraph"))
            .service(mock);

        // Without a `BatchQuery` extension, the request must go straight to the inner service
        let request = graphql_request("{ passthrough }", Context::new());
        let response = service.ready().await.unwrap().call(request).await.unwrap();

        let body = body::into_bytes(response.http_response.into_body())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({ "passthrough": true }))
        );

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_merges_batched_requests() {
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();

        let batch = Arc::new(Batch::spawn_handler(3));
        let mut requests = Vec::with_capacity(3);
        for index in 0..3 {
            let context = context_for_batch_index(&batch, index).await;
            requests.push(graphql_request(&format!("{{ slot{index} }}"), context));
        }

        let driver = tokio::task::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            let body = body::into_bytes(request.http_request.into_body())
                .await
                .unwrap();
            let merged: Vec<graphql::Request> = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                merged.iter().map(|r| r.query.clone()).collect::<Vec<_>>(),
                (0..3)
                    .map(|index| Some(format!("{{ slot{index} }}")))
                    .collect::<Vec<_>>()
            );

            let response_body = body::from_bytes(
                r#"[{"data":{"slot0":0}},{"data":{"slot1":1}},{"data":{"slot2":2}}]"#,
            );
            responder.send_response(HttpResponse {
                http_response: http::Response::builder().body(response_body).unwrap(),
                context: request.context,
            });
        });

        let service = ServiceBuilder::new()
            .layer(JoinBatchRequestsLayer::new("test_subgraph"))
            .service(mock);

        let futures = requests.into_iter().map(|request| {
            let mut service = service.clone();
            async move { service.ready().await.unwrap().call(request).await.unwrap() }
        });
        let responses = join_all(futures).await;

        for (index, response) in responses.into_iter().enumerate() {
            let body = body::into_bytes(response.http_response.into_body())
                .await
                .unwrap();
            let response: graphql::Response = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                response.data,
                Some(serde_json_bytes::json!({ format!("slot{index}"): index }))
            );
        }

        crate::plugin::test::await_mock_driver(driver).await;
    }

    #[rstest]
    #[case::wrong_length(r#"[{"data":{"slot0":0}},{"data":{"slot1":1}}]"#)]
    #[case::invalid_shape(r#"{"not":"an array"}"#)]
    #[tokio::test(flavor = "multi_thread")]
    async fn it_propagates_subgraph_error(#[case] bad_body: &'static str) {
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();

        let batch = Arc::new(Batch::spawn_handler(3));
        let mut requests = Vec::with_capacity(3);
        for index in 0..3 {
            let context = context_for_batch_index(&batch, index).await;
            requests.push(graphql_request(&format!("{{ slot{index} }}"), context));
        }

        let driver = tokio::task::spawn(async move {
            let (request, responder) = handle.next_request().await.unwrap();
            responder.send_response(HttpResponse {
                http_response: http::Response::builder()
                    .body(body::from_bytes(bad_body))
                    .unwrap(),
                context: request.context,
            });
        });

        let service = ServiceBuilder::new()
            .layer(JoinBatchRequestsLayer::new("test_subgraph"))
            .service(mock);

        let futures = requests.into_iter().map(|request| {
            let mut service = service.clone();
            async move { service.ready().await.unwrap().call(request).await }
        });
        let results = join_all(futures).await;

        for result in results {
            assert!(result.is_err(), "expected error response for {bad_body:?}");
        }

        crate::plugin::test::await_mock_driver(driver).await;
    }
}
