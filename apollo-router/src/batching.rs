//! Various utility functions and core structures used to implement batching support within
//! the router.
//!
//! Batching works roughly along these lines:
//! - At the router service, a batch query is split apart into multiple requests.
//! - A single [Batch] structure is created, which is responsible for handling the batch lifecycle.
//! - Each individual request gets a [BatchQuery] extension.
//! - After query planning of each individual request, the [PrepareBatchingExecutionLayer] collects
//!   the hashes of the plan nodes that will be executed unconditionally as part of the query plan.
//!   Those query nodes are candidates for being batched up into a single request at the subgraph
//!   side, as they do not depend on other data being fetched first.
//! - [BatchQuery::set_query_hashes] is called with those hashes, to set the expected number of
//!   subgraph requests. The hashes are also used to track successful and unsuccessful responses to
//!   each individual subgraph request.
//! - Once each subgraph request that is part of the batch reaches the subgraph service, instead of
//!   submitting the request to the HTTP client, it calls into [BatchQuery::signal_progress]. This
//!   is how subgraph requests are eventually batched up. When a [BatchQuery] has received all
//!   expected progress calls (per [BatchQuery::set_query_hashes]), that query is considered
//!   "finished".
//! - The [Batch] lifecycle handler receives messages from each [BatchQuery]. Once _all_
//!   [BatchQuery]s are finished, no more messages come in, and this is when the [Batch] lifecycle
//!   handler actually batches up the requests to each subgraph, and sends the batched subgraph
//!   requests to the HTTP client.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use futures::future::BoxFuture;
use http::StatusCode;
use opentelemetry::Context as otelContext;
use opentelemetry::trace::TraceContextExt;
use parking_lot::Mutex as PMutex;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower::BoxError;
use tracing::Instrument;
use tracing::Span;

use crate::Context;
use crate::error::FetchError;
use crate::error::SubgraphBatchingError;
use crate::graphql;
use crate::plugins::telemetry::otel::span_ext::OpenTelemetrySpanExt;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::execution;
use crate::services::process_batches;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::subgraph::SubgraphRequestId;
use crate::spec::QueryHash;

/// A query that is part of a batch.
/// Note: It's ok to make transient clones of this struct, but *do not* store clones anywhere apart
/// from the single copy in the extensions. The batching co-ordinator relies on the fact that all
/// senders are dropped to know when to finish processing.
#[derive(Clone, Debug)]
pub(crate) struct BatchQuery {
    /// The index of this query relative to the entire batch
    index: usize,

    /// A channel sender for sending updates to the entire batch
    sender: Arc<Mutex<Option<mpsc::Sender<BatchHandlerMessage>>>>,

    /// How many more progress updates are we expecting to send?
    remaining: Arc<AtomicUsize>,

    /// Batch to which this BatchQuery belongs
    batch: Arc<Batch>,
}

impl fmt::Display for BatchQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "index: {}, ", self.index)?;
        write!(f, "remaining: {}, ", self.remaining.load(Ordering::Acquire))?;
        write!(f, "sender: {:?}, ", self.sender)?;
        write!(f, "batch: {:?}, ", self.batch)?;
        Ok(())
    }
}

impl BatchQuery {
    /// Is this BatchQuery finished?
    pub(crate) fn finished(&self) -> bool {
        self.remaining.load(Ordering::Acquire) == 0
    }

    /// Inform the batch of query hashes representing fetches needed by this element of the batch query
    pub(crate) async fn set_query_hashes(
        &self,
        query_hashes: Vec<Arc<QueryHash>>,
    ) -> Result<(), BoxError> {
        self.remaining.store(query_hashes.len(), Ordering::Release);

        self.sender
            .lock()
            .await
            .as_ref()
            .ok_or(SubgraphBatchingError::SenderUnavailable)?
            .send(BatchHandlerMessage::Begin {
                index: self.index,
                query_hashes,
            })
            .await
            .map_err(|e| SubgraphBatchingError::ProcessingFailed(e.to_string()))?;
        Ok(())
    }

    /// Signal to the batch handler that this specific batch query has made some progress.
    ///
    /// The returned channel can be awaited to receive the GraphQL response, when ready.
    ///
    /// The HTTP client must be pre-readied.
    pub(crate) async fn signal_progress(
        &self,
        http_client: crate::services::http::BoxCloneService,
        request: SubgraphRequest,
    ) -> Result<oneshot::Receiver<Result<SubgraphResponse, BoxError>>, BoxError> {
        // Create a receiver for this query so that it can eventually get the request meant for it
        let (tx, rx) = oneshot::channel();

        tracing::debug!(
            "index: {}, REMAINING: {}",
            self.index,
            self.remaining.load(Ordering::Acquire)
        );
        self.sender
            .lock()
            .await
            .as_ref()
            .ok_or(SubgraphBatchingError::SenderUnavailable)?
            .send(BatchHandlerMessage::Progress(Box::new(
                BatchHandlerMessageProgress {
                    index: self.index,
                    http_client,
                    request,
                    response_sender: tx,
                    span_context: Span::current().context(),
                },
            )))
            .await
            .map_err(|e| SubgraphBatchingError::ProcessingFailed(e.to_string()))?;

        if !self.finished() {
            self.remaining.fetch_sub(1, Ordering::AcqRel);
        }

        // May now be finished
        if self.finished() {
            let mut sender = self.sender.lock().await;
            *sender = None;
        }

        Ok(rx)
    }

    /// Signal to the batch handler that this specific batch query is cancelled
    pub(crate) async fn signal_cancelled(&self, reason: String) -> Result<(), BoxError> {
        self.sender
            .lock()
            .await
            .as_ref()
            .ok_or(SubgraphBatchingError::SenderUnavailable)?
            .send(BatchHandlerMessage::Cancel {
                index: self.index,
                reason,
            })
            .await
            .map_err(|e| SubgraphBatchingError::ProcessingFailed(e.to_string()))?;

        if !self.finished() {
            self.remaining.fetch_sub(1, Ordering::AcqRel);
        }

        // May now be finished
        if self.finished() {
            let mut sender = self.sender.lock().await;
            *sender = None;
        }

        Ok(())
    }
}

// #[derive(Debug)]
enum BatchHandlerMessage {
    /// Cancel one of the batch items
    Cancel {
        index: usize,
        reason: String,
    },

    Progress(Box<BatchHandlerMessageProgress>),

    /// A query has passed query planning and knows how many fetches are needed
    /// to complete.
    Begin {
        index: usize,
        query_hashes: Vec<Arc<QueryHash>>,
    },
}

/// A query has reached the subgraph service and we should update its state
struct BatchHandlerMessageProgress {
    index: usize,
    http_client: crate::services::http::BoxCloneService,
    request: SubgraphRequest,
    response_sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
    span_context: otelContext,
}

/// Collection of info needed to resolve a batch query
pub(crate) struct BatchQueryInfo {
    /// The owning subgraph request
    request: SubgraphRequest,
    http_client: crate::services::http::BoxCloneService,
    /// Notifier for the subgraph service handler
    ///
    /// Note: This must be used or else the subgraph request will time out
    sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
}

// TODO: Do we want to generate a UUID for a batch for observability reasons?
// TODO: Do we want to track the size of a batch?
#[derive(Debug)]
pub(crate) struct Batch {
    /// A sender channel to communicate with the batching handler
    senders: PMutex<Vec<Option<mpsc::Sender<BatchHandlerMessage>>>>,

    /// The spawned batching handler task handle
    ///
    /// Note: We keep this as a failsafe. If the task doesn't terminate _before_ the batch is
    /// dropped, then we will abort() the task on drop.
    spawn_handle: JoinHandle<Result<(), BoxError>>,

    /// What is the size (number of input operations) of the batch?
    #[allow(dead_code)]
    size: usize,
}

impl Batch {
    /// Creates a new batch, spawning an async task for handling updates to the
    /// batch lifecycle.
    pub(crate) fn spawn_handler(size: usize) -> Self {
        tracing::debug!("New batch created with size {size}");

        // Create the message channel pair for sending update events to the spawned task
        let (spawn_tx, mut rx) = mpsc::channel(size);

        // Populate Senders
        let mut senders = vec![];

        for _ in 0..size {
            senders.push(Some(spawn_tx.clone()));
        }

        let spawn_handle = tokio::spawn(async move {
            /// Helper struct for keeping track of the state of each individual BatchQuery
            ///
            #[derive(Debug)]
            struct BatchQueryState {
                registered: HashSet<Arc<QueryHash>>,
                committed: HashSet<Arc<QueryHash>>,
                cancelled: HashSet<Arc<QueryHash>>,
            }

            impl BatchQueryState {
                // We are ready when everything we registered is in either cancelled or
                // committed.
                fn is_ready(&self) -> bool {
                    self.registered.difference(&self.committed.union(&self.cancelled).cloned().collect()).collect::<Vec<_>>().is_empty()
                }
            }

            // Progressively track the state of the various batch fetches that we expect to see. Keys are batch
            // indices.
            let mut batch_state: HashMap<usize, BatchQueryState> = HashMap::with_capacity(size);

            // We also need to keep track of all requests we need to make and their send handles
            let mut requests: Vec<Vec<BatchQueryInfo>> =
                Vec::from_iter((0..size).map(|_| Vec::new()));

            tracing::debug!("Batch about to await messages...");
            // Start handling messages from various portions of the request lifecycle
            // When recv() returns None, we want to stop processing messages
            while let Some(msg) = rx.recv().await {
                match msg {
                    BatchHandlerMessage::Cancel { index, reason } => {
                        // Log the reason for cancelling, update the state
                        tracing::debug!("Cancelling index: {index}, {reason}");

                        if let Some(state) = batch_state.get_mut(&index) {
                            // Short-circuit any requests that are waiting for this cancelled request to complete.
                            let cancelled_requests = std::mem::take(&mut requests[index]);
                            for BatchQueryInfo {
                                request, sender, ..
                            } in cancelled_requests
                            {
                                let subgraph_name = request.subgraph_name;
                                if let Err(log_error) = sender.send(Err(Box::new(FetchError::SubrequestBatchingError {
                                        service: subgraph_name.clone(),
                                        reason: format!("request cancelled: {reason}"),
                                    }))) {
                                    tracing::error!(service=subgraph_name, error=?log_error, "failed to notify waiter that request is cancelled");
                                }
                            }

                            // Clear out everything that has committed, now that they are cancelled, and
                            // mark everything as having been cancelled.
                            state.committed.clear();
                            state.cancelled = state.registered.clone();
                        }
                    }

                    BatchHandlerMessage::Begin {
                        index,
                        query_hashes,
                    } => {
                        tracing::debug!("Beginning batch for index {index} with {query_hashes:?}");

                        batch_state.insert(
                            index,
                            BatchQueryState {
                                cancelled: HashSet::with_capacity(query_hashes.len()),
                                committed: HashSet::with_capacity(query_hashes.len()),
                                registered: HashSet::from_iter(query_hashes),
                            },
                        );
                    }

                    BatchHandlerMessage::Progress(progress) => {
                        // Progress the index
                        let BatchHandlerMessageProgress {
                            index,
                            http_client,
                            request,
                            response_sender,
                            span_context,
                        } = *progress;

                        tracing::debug!("Progress index: {index}");

                        if let Some(state) = batch_state.get_mut(&index) {
                            state.committed.insert(request.query_hash.clone());
                        }

                        Span::current().add_link(span_context.span().span_context().clone());
                        requests[index].push(BatchQueryInfo {
                            http_client,
                            request,
                            sender: response_sender,
                        })
                    }
                }
            }

            // Make sure that we are actually ready and haven't forgotten to update something somewhere
            if batch_state.values().any(|f| !f.is_ready()) {
                tracing::error!("All senders for the batch have dropped before reaching the ready state: {batch_state:#?}");
                // There's not much else we can do, so perform an early return
                return Err(SubgraphBatchingError::ProcessingFailed("batch senders not ready when required".to_string()).into());
            }

            tracing::debug!("Assembling {size} requests into batches");

            // We now have a bunch of requests which are organised by index and we would like to
            // convert them into a bunch of requests organised by service...

            let all_in_one: Vec<_> = requests.into_iter().flatten().collect();

            // Now build up a Service oriented view to use in constructing our batches
            let mut svc_map: HashMap<String, Vec<BatchQueryInfo>> = HashMap::new();
            for BatchQueryInfo {
                http_client,
                request: sg_request,
                sender: tx,
            } in all_in_one
            {
                let subgraph_name = sg_request.subgraph_name.clone();
                let value = svc_map
                    .entry(
                        subgraph_name,
                    )
                    .or_default();
                value.push(BatchQueryInfo {
                    http_client,
                    request: sg_request,
                    sender: tx,
                });
            }

            process_batches(svc_map).await?;
            Ok(())
        }.instrument(tracing::info_span!("batch_request", size)));

        Self {
            senders: PMutex::new(senders),
            spawn_handle,
            size,
        }
    }

    /// Create a batch query for a specific index in this batch
    ///
    /// This function may fail if the index doesn't exist or has already been taken
    pub(crate) fn query_for_index(
        batch: Arc<Batch>,
        index: usize,
    ) -> Result<BatchQuery, SubgraphBatchingError> {
        let mut guard = batch.senders.lock();
        // It's a serious error if we try to get a query at an index which doesn't exist or which has already been taken
        if index >= guard.len() {
            return Err(SubgraphBatchingError::ProcessingFailed(format!(
                "tried to retriever sender for index: {index} which does not exist"
            )));
        }
        let opt_sender = std::mem::take(&mut guard[index]);
        if opt_sender.is_none() {
            return Err(SubgraphBatchingError::ProcessingFailed(format!(
                "tried to retriever sender for index: {index} which has already been taken"
            )));
        }
        drop(guard);
        Ok(BatchQuery {
            index,
            sender: Arc::new(Mutex::new(opt_sender)),
            remaining: Arc::new(AtomicUsize::new(0)),
            batch,
        })
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        // Failsafe: make sure that we kill the background task if the batch itself is dropped
        self.spawn_handle.abort();
    }
}

/// A batch of requests that we'll send to a subgraph (...as a single batch request).
pub(crate) struct SubgraphBatchRequest {
    pub(crate) http_client: crate::services::http::BoxCloneService,
    pub(crate) contexts: Vec<(Context, SubgraphRequestId)>,
    pub(crate) request: http::Request<RouterBody>,
    pub(crate) txs: Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
}

// Assemble a single batch request to a subgraph
pub(crate) async fn assemble_batch(
    batch_queries: Vec<BatchQueryInfo>,
) -> Result<SubgraphBatchRequest, BoxError> {
    let mut txs = Vec::with_capacity(batch_queries.len());
    let mut contexts = Vec::with_capacity(batch_queries.len());
    let mut graphql_bodies = Vec::with_capacity(batch_queries.len());

    let mut iter = batch_queries.into_iter();

    let first = iter.next().ok_or(SubgraphBatchingError::RequestsIsEmpty)?;
    let http_client = first.http_client;
    txs.push(first.sender);
    contexts.push((first.request.context, first.request.id));
    // We'll use the HTTP parts (headers, URI etc) from the first request for the whole batch
    let (parts, first_body) = first.request.subgraph_request.into_parts();
    graphql_bodies.push(first_body);

    for batch_query in iter {
        txs.push(batch_query.sender);
        contexts.push((batch_query.request.context, batch_query.request.id));
        graphql_bodies.push(batch_query.request.subgraph_request.into_body());
    }
    debug_assert_eq!(txs.len(), contexts.len());
    debug_assert_eq!(txs.len(), graphql_bodies.len());

    // Construct the actual byte body of the batched request
    let bytes = serde_json::to_vec(&graphql_bodies)?;

    // Generate the final request and pass it up
    let request = http::Request::from_parts(parts, router::body::from_bytes(bytes));
    Ok(SubgraphBatchRequest {
        http_client,
        contexts,
        request,
        txs,
    })
}

/// Handle pre-execution batching concerns:
/// - Inform the [BatchQuery] about the query plan fetch node hashes
/// - Reject the request if it contains any subscription or defer nodes
#[derive(Clone)]
pub(crate) struct PrepareBatchingExecutionLayer {
    _private: (),
}

impl PrepareBatchingExecutionLayer {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}

impl<S> tower::Layer<S> for PrepareBatchingExecutionLayer {
    type Service = PrepareBatchingExecutionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PrepareBatchingExecutionService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct PrepareBatchingExecutionService<S> {
    inner: S,
}

impl<S> tower::Service<execution::Request> for PrepareBatchingExecutionService<S>
where
    S: tower::Service<execution::Request, Response = execution::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: execution::Request) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        Box::pin(async move {
            let context = &req.context;
            let plan = &req.query_plan;
            let variables = &req.supergraph_request.body().variables;
            let is_deferred = plan.is_deferred(variables);
            let is_subscription = plan.is_subscription();

            let Some(batching) = context
                .extensions()
                .with_lock(|lock| lock.get::<crate::configuration::Batching>().cloned())
            else {
                return inner.call(req).await.map_err(Into::into);
            };

            if batching.enabled && (is_deferred || is_subscription) {
                let code = if is_deferred {
                    "BATCHING_DEFER_UNSUPPORTED"
                } else {
                    "BATCHING_SUBSCRIPTION_UNSUPPORTED"
                };
                let mut response = execution::Response::new_from_graphql_response(
                        graphql::Response::builder()
                        .error(crate::error::Error::builder()
                            .message("Deferred responses and subscriptions aren't supported in batches")
                            .extension_code(code)
                            .build())
                            .build(),
                        context.clone(),
                    );
                *response.response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                return Ok(response);
            }

            // Now perform query batch analysis
            let batch_query_opt = context
                .extensions()
                .with_lock(|lock| lock.get::<BatchQuery>().cloned());
            if let Some(batch_query) = batch_query_opt {
                let query_hashes = plan.query_hashes(batching, variables)?;
                batch_query.set_query_hashes(query_hashes).await?;
                tracing::debug!("batch registered: {}", batch_query);
            }

            inner.call(req).await.map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use http::header::ACCEPT;
    use http::header::CONTENT_TYPE;
    use http_body_util::BodyExt;
    use tokio::sync::oneshot;
    use tower::ServiceExt;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    use super::Batch;
    use super::BatchQueryInfo;
    use super::SubgraphBatchRequest;
    use super::assemble_batch;
    use crate::Context;
    use crate::TestHarness;
    use crate::graphql;
    use crate::graphql::Request;
    use crate::layers::ServiceExt as LayerExt;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::http::HttpClientServiceFactory;
    use crate::services::http::HttpRequest;
    use crate::services::http::HttpResponse;
    use crate::services::layers::content_negotiation::inject_subgraph_request_headers;
    use crate::services::router;
    use crate::services::router::body;
    use crate::services::subgraph;
    use crate::services::subgraph::SubgraphRequestId;
    use crate::services::subgraph::http::APPLICATION_JSON_HEADER_VALUE;
    use crate::spec::QueryHash;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_assembles_batch() {
        // Assemble a list of requests for testing
        let (receivers, requests): (Vec<_>, Vec<_>) = (0..2)
            .map(|index| {
                let (tx, rx) = oneshot::channel();
                let gql_request = graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build();

                (
                    rx,
                    BatchQueryInfo {
                        http_client: HttpClientServiceFactory::for_test("test"),
                        request: SubgraphRequest::fake_builder()
                            .subgraph_request(http::Request::builder().body(gql_request).unwrap())
                            .subgraph_name(format!("slot{index}"))
                            .build(),
                        sender: tx,
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
            std::str::from_utf8(&router::body::into_bytes(request.into_body()).await.unwrap())
                .unwrap(),
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
            let response = SubgraphResponse {
                response: http::Response::builder()
                    .body(graphql::Response::builder().data(data.clone()).build())
                    .unwrap(),
                context: Context::new(),
                subgraph_name: String::default(),
                id: SubgraphRequestId(String::new()),
            };

            tx.send(Ok(response)).unwrap();

            // We want to make sure that we don't hang the test if we don't get the correct message
            let received = tokio::time::timeout(Duration::from_millis(10), rx)
                .await
                .unwrap()
                .unwrap()
                .unwrap();

            assert_eq!(received.response.into_body().data, Some(data));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rejects_index_out_of_bounds() {
        let batch = Arc::new(Batch::spawn_handler(2));

        assert!(Batch::query_for_index(batch.clone(), 2).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rejects_duplicated_index_get() {
        let batch = Arc::new(Batch::spawn_handler(2));

        assert!(Batch::query_for_index(batch.clone(), 0).is_ok());
        assert!(Batch::query_for_index(batch.clone(), 0).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_limits_the_number_of_cancelled_sends() {
        let batch = Arc::new(Batch::spawn_handler(2));

        let bq = Batch::query_for_index(batch.clone(), 0).expect("its a valid index");

        assert!(
            bq.set_query_hashes(vec![Arc::new(QueryHash::default())])
                .await
                .is_ok()
        );
        assert!(!bq.finished());
        assert!(bq.signal_cancelled("why not?".to_string()).await.is_ok());
        assert!(bq.finished());
        assert!(
            bq.signal_cancelled("only once though".to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_limits_the_number_of_progressed_sends() {
        let (mock, handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let batch = Arc::new(Batch::spawn_handler(2));

        let bq = Batch::query_for_index(batch.clone(), 0).expect("its a valid index");

        let http_client = mock.boxed_clone();

        let request = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::default())
                    .unwrap(),
            )
            .subgraph_name("whatever".to_string())
            .build();
        assert!(
            bq.set_query_hashes(vec![Arc::new(QueryHash::default())])
                .await
                .is_ok()
        );
        assert!(!bq.finished());
        assert!(
            bq.signal_progress(http_client.clone(), request.clone())
                .await
                .is_ok()
        );
        assert!(bq.finished());
        assert!(bq.signal_progress(http_client, request).await.is_err());

        // We're only finishing one of two batch queries in this test,
        // so we should not see a subgraph request actually being sent.
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_limits_the_number_of_mixed_sends() {
        let (mock, handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let batch = Arc::new(Batch::spawn_handler(2));

        let bq = Batch::query_for_index(batch.clone(), 0).expect("its a valid index");

        let http_client = mock.boxed_clone();
        let request = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::default())
                    .unwrap(),
            )
            .subgraph_name("whatever".to_string())
            .build();
        assert!(
            bq.set_query_hashes(vec![Arc::new(QueryHash::default())])
                .await
                .is_ok()
        );
        assert!(!bq.finished());
        assert!(bq.signal_progress(http_client, request).await.is_ok());
        assert!(bq.finished());
        assert!(
            bq.signal_cancelled("only once though".to_string())
                .await
                .is_err()
        );

        // We're only finishing one of two batch queries in this test,
        // so we should not see a subgraph request actually being sent.
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_limits_the_number_of_mixed_sends_two_query_hashes() {
        let (mock, handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let batch = Arc::new(Batch::spawn_handler(2));

        let bq = Batch::query_for_index(batch.clone(), 0).expect("its a valid index");

        let http_client = mock.boxed_clone();
        let request = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::default())
                    .unwrap(),
            )
            .subgraph_name("whatever".to_string())
            .build();
        let qh = Arc::new(QueryHash::default());
        assert!(bq.set_query_hashes(vec![qh.clone(), qh]).await.is_ok());
        assert!(!bq.finished());
        assert!(bq.signal_progress(http_client, request).await.is_ok());
        assert!(!bq.finished());
        assert!(
            bq.signal_cancelled("only twice though".to_string())
                .await
                .is_ok()
        );
        assert!(bq.finished());
        assert!(
            bq.signal_cancelled("only twice though".to_string())
                .await
                .is_err()
        );

        // We're only finishing one of two batch queries in this test,
        // so we should not see a subgraph request actually being sent.
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    fn expect_batch(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // Extract info about this operation
        let (subgraph, count): (String, usize) = {
            let re = regex::Regex::new(r"entry([AB])\(count: ?([0-9]+)\)").unwrap();
            let captures = re.captures(requests[0].query.as_ref().unwrap()).unwrap();

            (captures[1].to_string(), captures[2].parse().unwrap())
        };

        // We should have gotten `count` elements
        assert_eq!(requests.len(), count);

        // Each element should have be for the specified subgraph and should have a field selection
        // of index.
        // Note: The router appends info to the query, so we append it at this check
        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__{}__0 {{ entry{}(count: {count}) {{ index }} }}",
                    subgraph.to_lowercase(),
                    subgraph
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..count)
                .map(|index| {
                    serde_json::json!({
                        "data": {
                            format!("entry{subgraph}"): {
                                "index": index
                            }
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_matches_subgraph_request_ids_to_responses() {
        // Create a wiremock server for each handler
        let mock_server = MockServer::start().await;
        mock_server
            .register(
                wiremock::Mock::given(matchers::method("POST"))
                    .and(matchers::path("/a"))
                    .respond_with(expect_batch)
                    .expect(1),
            )
            .await;

        let schema = include_str!("../tests/fixtures/batching/schema.graphql");
        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({
                "include_subgraph_errors": {
                    "all": true
                },
                "batching": {
                    "enabled": true,
                    "mode": "batch_http_link",
                    "subgraph": {
                        "all": {
                            "enabled": true
                        }
                    }
                },
                "override_subgraph_url": {
                    "a": format!("{}/a", mock_server.uri())
                },
            }))
            .unwrap()
            .schema(schema)
            .subgraph_hook(move |_subgraph_name, service| {
                service
                    .map_future_with_request_data(
                        |r: &subgraph::Request| r.id.clone(),
                        |id, f| async move {
                            let r: subgraph::ServiceResult = f.await;
                            assert_eq!(id, r.as_ref().map(|r| r.id.clone()).unwrap());
                            r
                        },
                    )
                    .boxed_clone()
            })
            .with_subgraph_network_requests()
            .build_router()
            .await
            .unwrap();

        let requests: Vec<_> = (0..3)
            .map(|index| {
                graphql::Request::builder()
                    .query(format!("query op{index}{{ entryA(count: 3) {{ index }} }}"))
                    .build()
            })
            .collect();
        let request = serde_json::to_value(requests).unwrap();

        let context = Context::new();
        let request = router::Request {
            context,
            router_request: http::Request::builder()
                .method("POST")
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(body::from_bytes(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        };

        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();

        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        insta::assert_json_snapshot!(response);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_sends_batched_request() {
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let batch = Arc::new(Batch::spawn_handler(2));

        let http_client = mock.boxed_clone();

        let query1 = Batch::query_for_index(batch.clone(), 0).unwrap();
        let query2 = Batch::query_for_index(batch.clone(), 1).unwrap();

        let request1 = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::builder().query("{ field1 }").build())
                    .unwrap(),
            )
            .subgraph_name("a")
            .build();

        let request2 = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::builder().query("{ field2 }").build())
                    .unwrap(),
            )
            .subgraph_name("a")
            .build();

        // We have to provide pre-readied HTTP clients.
        let client1 = http_client.clone().ready_oneshot().await.unwrap();
        let response1 = query1.signal_progress(client1, request1).await.unwrap();
        let client2 = http_client.clone().ready_oneshot().await.unwrap();
        let response2 = query2.signal_progress(client2, request2).await.unwrap();

        let (request, responder) =
            tokio::time::timeout(Duration::from_secs(5), handle.next_request())
                .await
                .expect("should get a request")
                .expect("service closed without request?");

        let body = request
            .http_request
            .into_body()
            .collect()
            .await
            .expect("should read subgraph request body");
        let (body1, body2) =
            serde_json::from_slice::<(graphql::Request, graphql::Request)>(&body.to_bytes())
                .expect("should have a two-element array json body");
        assert_eq!(body1.query.as_deref(), Some("{ field1 }"));
        assert_eq!(body2.query.as_deref(), Some("{ field2 }"));

        responder.send_response(HttpResponse {
            http_response: http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone())
                .body(router::body::from_bytes(
                    r#"[
                    { "data": { "field1": "value1" } },
                    { "data": { "field2": "value2" } }
                ]"#,
                ))
                .unwrap(),
            context: request.context,
        });

        // Check that the batch response was split up correctly
        let response1 = response1
            .await
            .expect("channel should be open")
            .expect("successful response");
        let response1 = response1.response.into_body();
        assert_eq!(
            response1.data,
            Some(serde_json_bytes::json!({ "field1": "value1" }))
        );

        let response2 = response2
            .await
            .expect("channel should be open")
            .expect("successful response");
        let response2 = response2.response.into_body();
        assert_eq!(
            response2.data,
            Some(serde_json_bytes::json!({ "field2": "value2" }))
        );

        // Only 1 call is expected
        drop(http_client);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_does_not_duplicate_headers_injected_by_subgraph_layer() {
        // Regression test: every request making up a batch has already passed through
        // `SubgraphLayer` (which injects Accept/Content-Type headers) before `call_http` diverts
        // it into batching via `signal_progress`. `process_batch` must not inject those headers a
        // second time, since `inject_subgraph_request_headers` appends (rather than replaces) the
        // Accept header.
        let (mock, mut handle) = tower_test::mock::pair::<HttpRequest, HttpResponse>();
        let batch = Arc::new(Batch::spawn_handler(2));

        let http_client = mock.boxed_clone();

        let query1 = Batch::query_for_index(batch.clone(), 0).unwrap();
        let query2 = Batch::query_for_index(batch.clone(), 1).unwrap();

        let mut request1 = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::builder().query("{ field1 }").build())
                    .unwrap(),
            )
            .subgraph_name("a")
            .build();
        inject_subgraph_request_headers(request1.subgraph_request.headers_mut());

        let mut request2 = SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(graphql::Request::builder().query("{ field2 }").build())
                    .unwrap(),
            )
            .subgraph_name("a")
            .build();
        inject_subgraph_request_headers(request2.subgraph_request.headers_mut());

        // We have to provide pre-readied HTTP clients.
        let client1 = http_client.clone().ready_oneshot().await.unwrap();
        let response1 = query1.signal_progress(client1, request1).await.unwrap();
        let client2 = http_client.clone().ready_oneshot().await.unwrap();
        let response2 = query2.signal_progress(client2, request2).await.unwrap();

        let (request, responder) =
            tokio::time::timeout(Duration::from_secs(5), handle.next_request())
                .await
                .expect("should get a request")
                .expect("service closed without request?");

        let headers = request.http_request.headers();
        let accept_values: Vec<_> = headers.get_all(ACCEPT).iter().collect();
        assert_eq!(
            accept_values.len(),
            1,
            "Accept header should not be duplicated, got: {accept_values:?}"
        );
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap(),
            &APPLICATION_JSON_HEADER_VALUE
        );

        responder.send_response(HttpResponse {
            http_response: http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone())
                .body(router::body::from_bytes(
                    r#"[
                    { "data": { "field1": "value1" } },
                    { "data": { "field2": "value2" } }
                ]"#,
                ))
                .unwrap(),
            context: request.context,
        });

        response1
            .await
            .expect("channel should be open")
            .expect("successful response");
        response2
            .await
            .expect("channel should be open")
            .expect("successful response");

        drop(http_client);
        crate::plugin::test::assert_no_mock_calls(handle).await;
    }
}
