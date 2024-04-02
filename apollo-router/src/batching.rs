//! An assembly of utility functions and core structures used to implement batching support within
//! the router.
//!
//! In addition to the core batching functionality, as expressed in `BatchQuery` and
//! `Batch`, there are a series of utility functions for efficiently converting
//! graphql Requests to/from batch representation in a variety of formats: JSON, bytes

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use hyper::Body;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context as otelContext;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower::BoxError;
use tracing::Instrument;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::error::FetchError;
use crate::graphql;
use crate::query_planner::fetch::QueryHash;
use crate::services::http::HttpClientServiceFactory;
use crate::services::process_batches;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Context;

/// A query that is part of a batch.
///
/// Note: We do NOT want this to implement `Clone` because it holds a sender
/// to the batch, which is waiting for all batch queries to drop their senders
/// in order to finish processing the batch.
#[derive(Debug)]
pub(crate) struct BatchQuery {
    /// The index of this query relative to the entire batch
    index: usize,

    /// A channel sender for sending updates to the entire batch
    sender: Option<mpsc::Sender<BatchHandlerMessage>>,

    /// How many more progress updates are we expecting to send?
    remaining: usize,
}

impl fmt::Display for BatchQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "index: {}, ", self.index)?;
        write!(f, "remaining: {}, ", self.remaining)?;
        write!(f, "sender: {:?}, ", self.sender)?;
        Ok(())
    }
}

impl BatchQuery {
    pub(crate) fn finished(&self) -> bool {
        self.remaining == 0
    }

    /// Inform the batch of query hashes representing fetches needed by this element of the batch query
    pub(crate) async fn set_query_hashes(&mut self, query_hashes: Vec<Arc<QueryHash>>) {
        self.remaining = query_hashes.len();

        // TODO: How should we handle the sender dying?
        self.sender
            .as_ref()
            .expect("set query hashes has a sender")
            .send(BatchHandlerMessage::Begin {
                index: self.index,
                query_hashes,
            })
            .await
            .expect("set query hashes could send");
    }

    /// Signal to the batch handler that this specific batch query has made some progress.
    ///
    /// The returned channel can be awaited to receive the GraphQL response, when ready.
    pub(crate) async fn signal_progress(
        &mut self,
        client_factory: HttpClientServiceFactory,
        request: SubgraphRequest,
        gql_request: graphql::Request,
    ) -> oneshot::Receiver<Result<SubgraphResponse, BoxError>> {
        // Create a receiver for this query so that it can eventually get the request meant for it
        let (tx, rx) = oneshot::channel();

        tracing::debug!("index: {}, REMAINING: {}", self.index, self.remaining);
        if self.sender.is_some() {
            // TODO: How should we handle the sender dying?
            self.sender
                .as_ref()
                .expect("signal progress has a sender")
                .send(BatchHandlerMessage::Progress {
                    index: self.index,
                    client_factory,
                    request,
                    gql_request,
                    response_sender: tx,
                    span_context: Span::current().context(),
                })
                .await
                .expect("signal progress could send");

            self.remaining -= 1;
            if self.remaining == 0 {
                self.sender = None;
            }
        }
        rx
    }

    /// Signal to the batch handler that this specific batch query is cancelled
    ///
    pub(crate) async fn signal_cancelled(&mut self, reason: String) {
        // TODO: How should we handle the sender dying?
        if self.sender.is_some() {
            self.sender
                .as_ref()
                .expect("signal cancelled has a sender")
                .send(BatchHandlerMessage::Cancel {
                    index: self.index,
                    reason,
                })
                .await
                .expect("signal cancelled could send");

            self.remaining -= 1;
            if self.remaining == 0 {
                self.sender = None;
            }
        } else {
            tracing::warn!("attempted to cancel completed batch query");
        }
    }
}

// #[derive(Debug)]
enum BatchHandlerMessage {
    /// Cancel one of the sub requests
    // TODO: How do we know which of the subfetches of the entire query to cancel? Is it all of them?
    Cancel { index: usize, reason: String },

    /// A query has reached the subgraph service and we should update its state
    Progress {
        index: usize,
        client_factory: HttpClientServiceFactory,
        request: SubgraphRequest,
        gql_request: graphql::Request,
        response_sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
        span_context: otelContext,
    },

    /// A query has passed query planning and now knows how many fetches are needed
    /// to complete.
    Begin {
        index: usize,
        query_hashes: Vec<Arc<QueryHash>>,
    },
}

/// Collection of info needed to resolve a batch query
pub(crate) struct BatchQueryInfo {
    /// The owning subgraph request
    request: SubgraphRequest,

    /// The GraphQL request tied to this subgraph request
    gql_request: graphql::Request,

    /// Notifier for the subgraph service handler
    ///
    /// Note: This must be used or else the subgraph request will time out
    sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
}

// #[derive(Debug)]
pub(crate) struct Batch {
    /// A sender channel to communicate with the batching handler
    senders: Mutex<Vec<Option<mpsc::Sender<BatchHandlerMessage>>>>,

    /// The spawned batching handler task handle
    ///
    /// Note: This _must_ be aborted or else the spawned task may run forever.
    spawn_handle: JoinHandle<()>,
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

            let mut master_client_factory = None;
            tracing::debug!("Batch about to await messages...");
            // Start handling messages from various portions of the request lifecycle
            // When recv() returns None, we want to stop processing message
            while let Some(msg) = rx.recv().await {
                match msg {
                    // Just clear out the fetch and error out the requests
                    BatchHandlerMessage::Cancel { index, reason } => {
                        // Log the reason for cancelling, update the state
                        tracing::info!("Cancelling index: {index}, {reason}");

                        // TODO: Handle missing index
                        if let Some(state) = batch_state.get_mut(&index) {
                            // Short-circuit any requests that are waiting for this cancelled request to complete.
                            let cancelled_requests = std::mem::take(&mut requests[index]);
                            for BatchQueryInfo {
                                request, sender, ..
                            } in cancelled_requests
                            {
                                sender
                                    .send(Err(Box::new(FetchError::SubrequestBatchingError {
                                        service: request
                                            .subgraph_name
                                            .expect("request has a subgraph_name"),
                                        reason: format!("request cancelled: {reason}"),
                                    })))
                                    .expect("batcher could send request cancelled to waiter");
                            }

                            // Clear out everything that has committed, now that they are cancelled, and
                            // mark everything as having been cancelled.
                            state.committed.clear();
                            state.cancelled = state.registered.clone();
                        }
                    }

                    // TODO: Do we want to handle if a query is outside of the range of the batch size?
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

                    // TODO: Do we want to handle if a query is outside of the range of the batch size?
                    BatchHandlerMessage::Progress {
                        index,
                        client_factory,
                        request,
                        gql_request,
                        response_sender,
                        span_context,
                    } => {
                        // Progress the index

                        tracing::info!("Progress index: {index}");

                        if let Some(state) = batch_state.get_mut(&index) {
                            state.committed.insert(request.query_hash.clone());
                        }

                        if master_client_factory.is_none() {
                            master_client_factory = Some(client_factory);
                        }
                        Span::current().add_link(span_context.span().span_context().clone());
                        requests[index].push(BatchQueryInfo {
                            request,
                            gql_request,
                            sender: response_sender,
                        })
                    }
                }
            }

            // Make sure that we are actually ready and haven't forgotten to update something somewhere
            if batch_state.values().any(|f| !f.is_ready()) {
                tracing::error!("All senders for the batch have dropped before reaching the ready state: {batch_state:#?}");
                panic!("all senders for the batch have dropped before reaching the ready state");
            }

            // TODO: Do we want to generate a UUID for a batch for observability reasons?
            tracing::debug!("Assembling {size} requests into batches");

            // We now have a bunch of requests which are organised by index and we would like to
            // convert them into a bunch of requests organised by service...
            // tracing::debug!("requests: {requests:?}");

            let all_in_one: Vec<_> = requests.into_iter().flatten().collect();
            // tracing::debug!("all_in_one: {all_in_one:?}");

            // Now build up a Service oriented view to use in constructing our batches
            let mut svc_map: HashMap<String, Vec<BatchQueryInfo>> = HashMap::new();
            for BatchQueryInfo {
                request: sg_request,
                gql_request,
                sender: tx,
            } in all_in_one
            {
                let value = svc_map
                    .entry(
                        sg_request
                            .subgraph_name
                            .clone()
                            .expect("request has a subgraph_name"),
                    )
                    .or_default();
                value.push(BatchQueryInfo {
                    request: sg_request,
                    gql_request,
                    sender: tx,
                });
            }

            // tracing::debug!("svc_map: {svc_map:?}");
            // If we don't have a master_client_factory, we can't do anything.
            if let Some(client_factory) = master_client_factory {
                process_batches(client_factory, svc_map)
                    .await
                    .expect("XXX NEEDS TO WORK FOR NOW");
            }
        }.instrument(tracing::info_span!("batch_request", size)));

        Self {
            senders: Mutex::new(senders),
            spawn_handle,
        }
    }

    /// Create a batch query for a specific index in this batch
    // TODO: Do we want to panic / error if the index is out of range?
    pub(crate) fn query_for_index(&self, index: usize) -> BatchQuery {
        let mut guard = self.senders.lock();
        let opt_sender = std::mem::take(&mut guard[index]);
        BatchQuery {
            index,
            sender: opt_sender,
            remaining: 0,
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        // Make sure that we kill the background task if the batch itself is dropped
        self.spawn_handle.abort();
    }
}

// Assemble a single batch request to a subgraph
pub(crate) async fn assemble_batch(
    // context: Context,
    requests: Vec<BatchQueryInfo>,
) -> (
    String,
    Context,
    http::Request<Body>,
    Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
) {
    // Extract the collection of parts from the requests
    let (txs, request_pairs): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .map(|r| (r.sender, (r.request, r.gql_request)))
        .unzip();
    let (requests, gql_requests): (Vec<_>, Vec<_>) = request_pairs.into_iter().unzip();

    // Construct the actual byte body of the batched request
    let bytes = hyper::body::to_bytes(
        serde_json::to_string(&gql_requests).expect("JSON serialization should not fail"),
    )
    .await
    .expect("byte serialization should not fail");

    // Grab the common info from the first request
    let context = requests
        .first()
        .expect("batch to assemble had no requests")
        .context
        .clone();
    let first_request = requests
        .into_iter()
        .next()
        .expect("batch to assemble had no requests")
        .subgraph_request;
    let operation_name = first_request
        .body()
        .operation_name
        .clone()
        .unwrap_or_default();
    let (parts, _) = first_request.into_parts();

    // Generate the final request and pass it up
    // TODO: The previous implementation reversed the txs here. Is that necessary?
    let request = http::Request::from_parts(parts, Body::from(bytes));
    (operation_name, context, request, txs)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hyper::body::to_bytes;
    use tokio::sync::oneshot;

    use super::assemble_batch;
    use super::BatchQueryInfo;
    use crate::graphql;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

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
                        request: SubgraphRequest::fake_builder()
                            .subgraph_request(
                                http::Request::builder().body(gql_request.clone()).unwrap(),
                            )
                            .subgraph_name(format!("slot{index}"))
                            .build(),
                        gql_request,
                        sender: tx,
                    },
                )
            })
            .unzip();

        // Assemble them
        let (op_name, _context, request, txs) = assemble_batch(requests).await;

        // Make sure that the name of the entire batch is that of the first
        assert_eq!(op_name, "batch_test_0");

        // We should see the aggregation of all of the requests
        let actual: Vec<graphql::Request> = serde_json::from_str(
            &String::from_utf8(to_bytes(request.into_body()).await.unwrap().to_vec()).unwrap(),
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
}
